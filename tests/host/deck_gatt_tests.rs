use crate::deck::proto::{self, LineAssembler};
use crate::deck_gatt;

#[test]
fn channel_five_line_round_trips_over_default_att_mtu() {
    let json = r#"{"m":"transcript","agent":1,"text":"你好，今天想看哪个助手的回复？","final":1}"#
        .as_bytes();
    assert!(json.len() > proto::PAYLOAD_MAX);

    let mut assembler = LineAssembler::new();
    let mut lines = Vec::new();
    let mut offset = 0;
    let mut report = [0; proto::REPORT_LEN];
    while offset < json.len() + 1 {
        let count = proto::frame(json, offset, &mut report);
        assert!(count > 0);
        offset += count;
        for notification in deck_gatt::notifications(&report, 23) {
            assert!((1..=20).contains(&notification.len()));
            let mut body = [0; proto::REPORT_LEN];
            let len = deck_gatt::report_body(notification, &mut body);
            assembler.push(&body[..len], &mut |line| lines.push(line.unwrap().to_vec()));
        }
    }
    assert_eq!(lines, [json.to_vec()]);
}

#[test]
fn rx_ignores_empty_and_oversized_values() {
    let mut frame = [0xa5; proto::REPORT_LEN];
    assert_eq!(deck_gatt::report_body(&[], &mut frame), 0);
    assert_eq!(
        deck_gatt::report_body(&[0; proto::PAYLOAD_MAX + 1], &mut frame),
        0
    );
    assert_eq!(frame, [0xa5; proto::REPORT_LEN]);
}

#[test]
fn large_mtu_sends_one_value_without_hid_padding() {
    let json = br#"{"m":"synced"}"#;
    let mut report = [0; proto::REPORT_LEN];
    proto::frame(json, 0, &mut report);
    let mut values = deck_gatt::notifications(&report, 247);
    assert_eq!(values.next(), Some(b"{\"m\":\"synced\"}\n".as_slice()));
    assert_eq!(values.next(), None);
}
