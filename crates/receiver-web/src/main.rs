use axum::{
    extract::{Path, State},
    http::StatusCode,
    routing::{get, post},
    Json, Router,
};
use receiver_core::{
    recording::{OutputOptions, RecordingStatus},
    session::{Config, Session},
    source,
};
use serde_json::{json, Value};
use std::{
    env, fs,
    net::SocketAddr,
    path::{Path as FsPath, PathBuf},
    sync::{Arc, Mutex},
};
use tower_http::{services::ServeDir, trace::TraceLayer};

const MAX_PREFERENCES_BYTES: u64 = 128 * 1024;

#[derive(Clone)]
struct AppState {
    receiver: Arc<Mutex<Option<Session>>>,
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
            let receiver = state.receiver.lock().map_err(|e| e.to_string())?;
            match receiver.as_ref() {
                None => Ok(Value::Null),
                Some(session) => {
                    let (status, audio) =
                        session.snapshot_with_monitor(selected.as_deref(), automatic);
                    Ok(json!({ "status": status, "audio": audio }))
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
