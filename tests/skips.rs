// SPDX-License-Identifier: GPL-3.0-or-later

//! `common::skip!` (#207): a guard's skip must show, under the name CI's list uses.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

mod common;

use std::process::Command;

const CHILD: &str = "SACDFS_SKIP_TEST_CHILD";

#[test]
fn a_test_is_named_by_its_binary_and_function() {
    assert_eq!(common::test_name!(), "skips::a_test_is_named_by_its_binary_and_function");
}

/// Runs itself in a child, whose skip therefore doesn't count in this run. Captured or on one
/// thread, the line must reach stderr, and the test must stop there.
#[test]
fn a_skip_is_printed_past_the_capture_and_returns() {
    if std::env::var_os(CHILD).is_some() {
        common::skip!("the reason");
    }
    for threads in ["1", "2"] {
        let out = Command::new(std::env::current_exe().unwrap())
            .env(CHILD, "1")
            .args(["--exact", "a_skip_is_printed_past_the_capture_and_returns", "--test-threads", threads])
            .output()
            .unwrap();
        assert!(out.status.success(), "{out:?}");
        let stderr = String::from_utf8(out.stderr).unwrap();
        let want = "SKIPPED skips::a_skip_is_printed_past_the_capture_and_returns: the reason\n";
        assert_eq!(stderr, want, "with {threads} test threads");
    }
}
