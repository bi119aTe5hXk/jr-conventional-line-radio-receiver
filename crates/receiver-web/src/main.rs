use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use receiver_core::{
    pipeline::Audio,
    recording::{OutputOptions, RecordingStatus},
    session::{Config, Session},
    source,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    collections::{HashMap, VecDeque},
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
};
use tower_http::{services::ServeDir, trace::TraceLayer};

const MAX_PREFERENCES_BYTES: u64 = 128 * 1024;
const AUDIO_HISTORY_FRAMES: usize = 40;

#[derive(Clone, Deserialize)]
struct AudioCursor {
    channel: String,
    epoch: u64,
    index: u32,
}

#[derive(Default)]
struct AudioHub {
    generation: u64,
    channels: HashMap<String, VecDeque<Audio>>,
}
impl AudioHub {
    fn clear(&mut self) {
        self.generation = self.generation.wrapping_add(1);
        self.channels.clear();
    }

    fn publish(&mut self, frames: Vec<Audio>) {
        for frame in frames {
            let history = self.channels.entry(frame.channel.clone()).or_default();
            if history.back().is_some_and(|last| last.epoch != frame.epoch) {
                history.clear();
            }
            if history
                .back()
                .is_some_and(|last| last.epoch == frame.epoch && last.index >= frame.index)
            {
                continue;
            }
            history.push_back(frame);
            while history.len() > AUDIO_HISTORY_FRAMES {
                history.pop_front();
            }
        }
    }

    fn read(
        &self,
        channel: Option<&str>,
        after: Option<&AudioCursor>,
        generation: Option<u64>,
    ) -> Vec<Audio> {
        let Some(channel) = channel else {
            return Vec::new();
        };
        let Some(history) = self.channels.get(channel) else {
            return Vec::new();
        };
        let after = after.filter(|cursor| {
            generation == Some(self.generation) && cursor.channel.as_str() == channel
        });
        history
            .iter()
            .filter(|frame| match after {
                None => true,
                Some(cursor) => {
                    frame.epoch > cursor.epoch
                        || (frame.epoch == cursor.epoch && frame.index > cursor.index)
                }
            })
            .cloned()
            .collect()
    }
}

#[derive(Clone)]
struct AppState {
    receiver: Arc<Mutex<Option<Session>>>,
    audio: Arc<Mutex<AudioHub>>,
    preferences_path: Arc<PathBuf>,
    recordings_dir: Arc<PathBuf>,
}
type ApiResult = Result<Json<Value>, (StatusCode, Json<Value>)>;

fn api_error(error: impl ToString) -> (StatusCode, Json<Value>) {
    (
        StatusCode::BAD_REQUEST,
        Json(json!({ "error": error.to_string() })),
    )
}

fn argument<T: serde::de::DeserializeOwned>(args: &Value, name: &str) -> Result<T, String> {
    serde_json::from_value(
        args.get(name)
            .cloned()
            .ok_or_else(|| format!("Missing argument: {name}"))?,
    )
    .map_err(|e| format!("Invalid argument {name}: {e}"))
}

async fn health() -> &'static str {
    "ok"
}

async fn invoke(
    Path(command): Path<String>,
    State(state): State<AppState>,
    Json(args): Json<Value>,
) -> ApiResult {
    let result = tokio::task::spawn_blocking(move || invoke_blocking(&command, &args, &state))
        .await
        .map_err(api_error)?
        .map_err(api_error)?;
    Ok(Json(result))
}

fn invoke_blocking(command: &str, args: &Value, state: &AppState) -> Result<Value, String> {
    match command {
        "devices" => serde_json::to_value(source::enumerate()?).map_err(|e| e.to_string()),
        "presets" => {
            let region: String = argument(args, "region")?;
            serde_json::to_value(receiver_core::presets::resolve(&region)?.channels)
                .map_err(|e| e.to_string())
        }
        "preset_catalog" => {
            let path: Option<String> = args
                .get("path")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten();
            let catalog = match path {
                Some(path) => receiver_core::presets::load_file(FsPath::new(&path))?,
                None => receiver_core::presets::catalog()?.to_vec(),
            };
            serde_json::to_value(catalog).map_err(|e| e.to_string())
        }
        "load_preferences" => load_preferences(&state.preferences_path),
        "save_preferences" => {
            let settings = args
                .get("settings")
                .cloned()
                .ok_or("Missing argument: settings")?;
            save_preferences(&state.preferences_path, &settings)?;
            Ok(Value::Null)
        }
        "default_recording_directory" => Ok(Value::String(
            state.recordings_dir.to_string_lossy().into_owned(),
        )),
        "start" => {
            let config: Config = argument(args, "config")?;
            let mut receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            if receiver
                .as_ref()
                .is_some_and(|session| session.snapshot(None).0.running)
            {
                return Err("受信中です。停止してから設定を変更してください。".into());
            }
            *receiver = Some(Session::start(config)?);
            drop(receiver);
            state.audio.lock().map_err(|e| e.to_string())?.clear();
            Ok(Value::Null)
        }
        "stop" => {
            let mut receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            let status = if let Some(session) = receiver.as_mut() {
                session.stop();
                session.stop_recording()
            } else {
                RecordingStatus::default()
            };
            drop(receiver);
            state.audio.lock().map_err(|e| e.to_string())?.clear();
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "start_recording" => {
            let directory: String = argument(args, "directory")?;
            let options: Option<OutputOptions> = args
                .get("options")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| e.to_string())?;
            let receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            let session = receiver
                .as_ref()
                .ok_or("受信を開始してから録音してください。")?;
            session
                .start_recording_with_options(FsPath::new(&directory), options.unwrap_or_default())
                .and_then(|status| serde_json::to_value(status).map_err(|e| e.to_string()))
        }
        "stop_recording" => {
            let receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            let status = receiver
                .as_ref()
                .ok_or("録音中の受信セッションがありません。")?
                .stop_recording();
            serde_json::to_value(status).map_err(|e| e.to_string())
        }
        "snapshot" => {
            let selected: Option<String> = args
                .get("selected")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten();
            let automatic = args
                .get("automatic")
                .and_then(Value::as_bool)
                .unwrap_or(false);
            let audio_after: Option<AudioCursor> = args
                .get("audio_after")
                .cloned()
                .map(serde_json::from_value)
                .transpose()
                .map_err(|e| e.to_string())?
                .flatten();
            let audio_generation = args.get("audio_generation").and_then(Value::as_u64);
            let receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            match receiver.as_ref() {
                None => Ok(Value::Null),
                Some(session) => {
                    let (status, fresh_audio) =
                        session.snapshot_with_monitor(selected.as_deref(), automatic);
                    drop(receiver);
                    let mut audio = state.audio.lock().map_err(|e| e.to_string())?;
                    audio.publish(fresh_audio);
                    let frames = audio.read(
                        status.selected_channel.as_deref(),
                        audio_after.as_ref(),
                        audio_generation,
                    );
                    Ok(json!({
                        "status": status,
                        "audio": frames,
                        "audio_generation": audio.generation
                    }))
                }
            }
        }
        // Native file dialogs do not exist in a browser. Paths remain editable
        // text fields, and container defaults are returned by the APIs above.
        "plugin:dialog|open" => Ok(Value::Null),
        _ => Err(format!("Unknown command: {command}")),
    }
}

fn load_preferences(path: &FsPath) -> Result<Value, String> {
    match fs::metadata(path) {
        Ok(metadata) if metadata.len() > MAX_PREFERENCES_BYTES => {
            return Err("設定ファイルが大きすぎます。".into())
        }
        Ok(_) => {}
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(Value::Null),
        Err(e) => return Err(format!("設定ファイルを読み込めません：{e}")),
    }
    let bytes = fs::read(path).map_err(|e| format!("設定ファイルを読み込めません：{e}"))?;
    serde_json::from_slice(&bytes).map_err(|e| format!("設定ファイルの JSON 形式が不正です：{e}"))
}

fn save_preferences(path: &FsPath, settings: &Value) -> Result<(), String> {
    if settings.get("version").and_then(Value::as_u64) != Some(1) {
        return Err("このバージョンの設定ファイルには対応していません。".into());
    }
    let mut bytes = serde_json::to_vec_pretty(settings).map_err(|e| e.to_string())?;
    bytes.push(b'\n');
    if bytes.len() as u64 > MAX_PREFERENCES_BYTES {
        return Err("設定のサイズが大きすぎます。".into());
    }
    let parent = path
        .parent()
        .ok_or("設定フォルダーを取得できませんでした。")?;
    fs::create_dir_all(parent).map_err(|e| format!("設定フォルダーを作成できません：{e}"))?;
    let temporary = parent.join(format!(".settings-{}.tmp", std::process::id()));
    fs::write(&temporary, bytes).map_err(|e| format!("設定を書き込めません：{e}"))?;
    fs::rename(&temporary, path).map_err(|e| format!("設定を保存できません：{e}"))
}

#[tokio::main]
async fn main() {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_default_env()
                .unwrap_or_else(|_| "jr_receiver_web=info,tower_http=info".into()),
        )
        .init();

    let listen = env::var("JR_RECEIVER_LISTEN").unwrap_or_else(|_| "0.0.0.0:8080".into());
    let ui_dir = env::var_os("JR_RECEIVER_UI_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("ui"));
    let config_dir = env::var_os("JR_RECEIVER_CONFIG_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/config"));
    let recordings_dir = env::var_os("JR_RECEIVER_RECORDINGS_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("data/recordings"));
    fs::create_dir_all(&recordings_dir).expect("Unable to create recording directory");

    let state = AppState {
        receiver: Arc::new(Mutex::new(None)),
        audio: Arc::new(Mutex::new(AudioHub::default())),
        preferences_path: Arc::new(config_dir.join("settings.json")),
        recordings_dir: Arc::new(recordings_dir),
    };
    let shutdown_state = state.clone();
    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/invoke/{command}", post(invoke))
        .fallback_service(ServeDir::new(ui_dir).append_index_html_on_directories(true))
        .layer(TraceLayer::new_for_http())
        .with_state(state);
    let address: SocketAddr = listen.parse().expect("Invalid JR_RECEIVER_LISTEN address");
    let listener = tokio::net::TcpListener::bind(address)
        .await
        .expect("Unable to bind web server");
    tracing::info!(%address, "JR receiver web server is listening");
    axum::serve(listener, app)
        .with_graceful_shutdown(async move {
            let _ = tokio::signal::ctrl_c().await;
            if let Ok(mut receiver) = shutdown_state.receiver.lock() {
                if let Some(session) = receiver.as_mut() {
                    session.stop();
                }
            }
        })
        .await
        .expect("Web server failed");
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(channel: &str, epoch: u64, index: u32) -> Audio {
        Audio {
            channel: channel.into(),
            epoch,
            index,
            pcm: vec![index as i16],
        }
    }

    #[test]
    fn each_browser_cursor_reads_the_same_published_audio_once() {
        let mut hub = AudioHub::default();
        hub.publish(vec![frame("a", 2, 10), frame("a", 2, 11)]);
        assert_eq!(hub.read(Some("a"), None, None).len(), 2);
        assert_eq!(hub.read(Some("a"), None, None).len(), 2);
        let cursor = AudioCursor {
            channel: "a".into(),
            epoch: 2,
            index: 11,
        };
        assert!(hub.read(Some("a"), Some(&cursor), Some(0)).is_empty());
        hub.publish(vec![frame("a", 2, 12)]);
        assert_eq!(hub.read(Some("a"), Some(&cursor), Some(0))[0].index, 12);
    }

    #[test]
    fn a_new_receiver_generation_invalidates_old_browser_cursors() {
        let mut hub = AudioHub::default();
        hub.publish(vec![frame("a", 4, 99)]);
        let old = AudioCursor {
            channel: "a".into(),
            epoch: 4,
            index: 99,
        };
        hub.clear();
        hub.publish(vec![frame("a", 0, 1)]);
        let replay = hub.read(Some("a"), Some(&old), Some(0));
        assert_eq!(replay.len(), 1);
        assert_eq!(replay[0].index, 1);
    }

    #[test]
    fn audio_history_is_bounded_per_channel() {
        let mut hub = AudioHub::default();
        hub.publish(
            (0..AUDIO_HISTORY_FRAMES as u32 + 5)
                .map(|index| frame("a", 1, index))
                .collect(),
        );
        let frames = hub.read(Some("a"), None, None);
        assert_eq!(frames.len(), AUDIO_HISTORY_FRAMES);
        assert_eq!(frames[0].index, 5);
    }
}
