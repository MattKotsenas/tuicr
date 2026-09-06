pub(super) struct TestReviewsDir {
    _dir: tempfile::TempDir,
}

impl TestReviewsDir {
    pub(super) fn new() -> Self {
        let dir = tempfile::tempdir().expect("failed to create test reviews dir");
        crate::persistence::storage::set_test_reviews_dir(Some(dir.path().to_path_buf()));
        Self { _dir: dir }
    }
}

impl Drop for TestReviewsDir {
    fn drop(&mut self) {
        crate::persistence::storage::set_test_reviews_dir(None);
    }
}

mod change_status_tests;
mod commit_scoped_comment_tests;
mod commit_selection_tests;
mod decoration_skip_tests;
mod diff_reload_tests;
mod diff_search_tests;
mod diff_source_tests;
mod diff_watch_tests;
mod expand_gap_tests;
mod file_filter_tests;
mod find_source_line_tests;
mod persistence_merge_tests;
mod pr_info_tests;
mod render_perf_tests;
mod sbs_comment_side_tests;
mod scroll_behavior_tests;
mod scroll_tests;
mod sessions_resume_tests;
mod single_file_view_tests;
mod submit_flow_tests;
mod target_selector_tests;
mod tree_tests;
mod visual_selection_tests;
