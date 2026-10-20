use agently_cli_mcp::cli_proc::CliOutcome;
use agently_cli_mcp::cli_proc::status::classify;

#[test]
fn confirmation_is_detected_before_exit_code() {
    let out = br#"{"ok":true,"data":{"confirmation_required":true,"confirmation_token":"ctk_1","summary":{"action":"send"}}}"#;
    match classify(out, "", Some(0)).unwrap() {
        CliOutcome::ConfirmationRequired { token, .. } => assert_eq!(token, "ctk_1"),
        _ => panic!("expected confirmation"),
    }
}

#[test]
fn ok_false_with_exit_zero_is_an_error() {
    let out =
        br#"{"ok":false,"error":{"type":"api_error","code":404,"message":"Resource not found"}}"#;
    let err = classify(out, "", Some(0)).unwrap_err();
    assert_eq!(err.code(), "NOT_FOUND");
}

#[test]
fn exit_one_plain_stderr_is_invalid_argument() {
    let err = classify(b"", "--limit must be between 1 and 50\n", Some(1)).unwrap_err();
    assert_eq!(err.code(), "INVALID_ARGUMENT");
    assert!(err.to_string().contains("--limit"));
}

#[test]
fn non_json_is_protocol_error_without_echoing_stdout() {
    let err = classify(b"secret body", "boom", Some(0)).unwrap_err();
    assert_eq!(err.code(), "PROTOCOL_ERROR");
    assert!(!format!("{err}").contains("secret body"));
}
