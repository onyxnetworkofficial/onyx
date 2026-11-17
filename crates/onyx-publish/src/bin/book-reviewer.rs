//! `book-reviewer`: the signed book reviewer's tool for publisher
//! applications (ADR-0066 item 8; see `book_reviewer.rs` and
//! docs/ops/publisher-review.md). Test network only.

fn main() -> std::process::ExitCode {
    onyx_publish::reviewer_main()
}
