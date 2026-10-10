use receiver_core::{
    data::Format,
    frame::Synchronizer,
    pipeline::{presets, validate_channels},
};

#[test]
fn synchronization_requires_three_envelopes_and_drops_after_three_failures() {
    let mut envelope = [0u8; 48];
    for (i, m) in [0x3d22u16, 0xe867, 0xe867].iter().enumerate() {
        envelope[16 * i..16 * i + 2].copy_from_slice(&m.to_be_bytes());
    }
    let mut s = Synchronizer::default();
    assert!(s.packed(&envelope).is_empty());
    assert!(s.packed(&envelope).is_empty());
    assert_eq!(s.packed(&envelope).len(), 3);
    assert!(s.locked);
    for _ in 0..3 {
        assert_eq!(s.packed(&[0; 48]).len(), 1);
    }
    assert!(!s.locked);
}
#[test]
fn chart_has_22_channels_and_checks_actual_capture_bandwidth() {
    let c = presets("east");
    assert_eq!(c.len(), 22);
    assert_eq!(c[0].frequency_hz, 352537500.0);
    assert_eq!(c[21].frequency_hz, 352668750.0);
    assert!(validate_channels(&c, 240000.0, 352603125.0).is_ok());
    assert!(validate_channels(&c, 96000.0, 352603125.0).is_err());
    assert_eq!(presets("west")[1].format, Format::Common);
    assert!(presets("central")
        .iter()
        .all(|c| c.format == Format::Slotted));
}
