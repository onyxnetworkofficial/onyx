//! `onyx-publish`: the publisher's tool (see the header of `lib.rs` and
//! docs/ops/bulk-publish.md).

fn main() -> std::process::ExitCode {
    onyx_publish::publish_main()
}
