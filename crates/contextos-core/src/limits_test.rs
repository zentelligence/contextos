use super::*;

#[test]
fn the_request_body_limit_holds_the_largest_file_once_base64_encoded_with_room_for_its_envelope() {
    let largest_encoded = MAX_FILE_BYTES.div_ceil(3) * 4;

    assert_eq!(MAX_REQUEST_BODY_BYTES, 14 * 1024 * 1024);
    assert!(MAX_REQUEST_BODY_BYTES >= largest_encoded + REQUEST_ENVELOPE_BYTES);
}
