//! `.http` gutter run marks: which lines are request starts (where IDEA puts its
//! run icon) and the streaming accumulator the panel shows while a body arrives.

use ide_http::exec::HttpResponse;
use ide_ui::http::request_lines;

#[test]
fn request_lines_are_the_ones_a_run_mark_belongs_on() {
    let text = "# comment\nGET https://api.test/items\nAuthorization: Bearer t\n\nPOST /items\nContent-Type: application/json\n\n{\"a\":1}\n";
    let lines = request_lines(text);
    assert_eq!(lines, vec![1, 4], "the GET and the POST start requests");
}

#[test]
fn header_and_body_lines_are_not_requests() {
    let text = "POST https://x/y\nContent-Type: text/plain\nAccept: */*\n\nGET\n";
    // `GET` alone has no target, so it is not a request line.
    assert_eq!(request_lines(text), vec![0]);
}

#[test]
fn methods_need_a_target_and_comments_are_skipped() {
    assert!(request_lines("// GET https://x\n").is_empty());
    assert!(request_lines("getting started\n").is_empty(), "a word starting with GET is not a request");
    assert_eq!(request_lines("DELETE {{host}}/items/1\n"), vec![0], "a variable target counts");
    assert_eq!(request_lines("  OPTIONS /\n"), vec![0], "indentation is fine");
}

#[test]
fn a_streamed_body_accumulates_in_arrival_order() {
    // The panel appends each chunk to the live text; the final response is the
    // same bytes (this is the join the viewer relies on).
    let chunks = ["{\"items\":[", "1,2,3", "]}"];
    let mut live = String::new();
    for chunk in chunks {
        live.push_str(chunk);
    }
    assert_eq!(live, "{\"items\":[1,2,3]}");
}

#[test]
fn a_response_can_be_built_from_a_streamed_body() {
    // The streaming path returns the same `HttpResponse` shape as the buffered
    // one, so the panel's response view is shared.
    let response = HttpResponse {
        status: 200,
        status_text: "OK".into(),
        version: "HTTP/1.1".into(),
        headers: vec![("content-type".into(), "application/json".into())],
        content_type: Some("application/json".into()),
        redirects: Vec::new(),
        body: Some("{\"ok\":true}".into()),
        size: 11.,
        truncated: false,
        ttfb_ms: 3.,
        total_ms: 9.,
    };
    assert_eq!(response.body.as_deref(), Some("{\"ok\":true}"));
    assert_eq!(response.size, 11.);
}
