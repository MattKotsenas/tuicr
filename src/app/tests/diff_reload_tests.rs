use super::TestReviewsDir;
use crate::app::*;
use crate::model::FileStatus;
use std::cell::RefCell;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

/// Fake `VcsBackend` whose fetch results are queued per call, so a test can
/// return different diffs across successive fetches.
///
/// `VcsBackend` methods take `&self`, so the queue and call counter need
/// interior mutability. The counter is an `Arc<AtomicUsize>`, not `Rc<Cell<_>>`:
/// `VcsBackend: Send` (`src/vcs/traits.rs`) rules out `Rc`.
struct ScriptedVcs {
    info: VcsInfo,
    working_tree_diff_results: RefCell<VecDeque<Result<Vec<DiffFile>>>>,
    working_tree_diff_calls: Arc<AtomicUsize>,
    /// One entry per fetch: whether that call was handed a highlighter with no
    /// grammars loaded. Lets a test check which fetch got the cheap parse.
    grammar_counts: Arc<Mutex<Vec<usize>>>,
    commit_range_diff_results: RefCell<VecDeque<Result<Vec<DiffFile>>>>,
    /// The commit ids each `get_commit_range_diff` call was asked for, in
    /// call order, so a test can tell a narrowed subrange fetch apart from a
    /// full-range one.
    commit_range_diff_ids: Arc<Mutex<Vec<Vec<String>>>>,
}

impl ScriptedVcs {
    fn new() -> Self {
        Self::with_info(test_vcs_info())
    }

    fn with_info(info: VcsInfo) -> Self {
        Self {
            info,
            working_tree_diff_results: RefCell::new(VecDeque::new()),
            working_tree_diff_calls: Arc::new(AtomicUsize::new(0)),
            grammar_counts: Arc::new(Mutex::new(Vec::new())),
            commit_range_diff_results: RefCell::new(VecDeque::new()),
            commit_range_diff_ids: Arc::new(Mutex::new(Vec::new())),
        }
    }

    /// Queue one `get_working_tree_diff` response. Responses are returned in
    /// FIFO order, one per call; calling past the end of the queue panics so
    /// an unexpectedly-extra fetch fails loudly instead of blocking forever.
    fn push_working_tree_diff(&self, result: Result<Vec<DiffFile>>) {
        self.working_tree_diff_results
            .borrow_mut()
            .push_back(result);
    }

    /// Queue one `get_commit_range_diff` response. Same FIFO/panic-past-the-end
    /// contract as `push_working_tree_diff`.
    fn push_commit_range_diff(&self, result: Result<Vec<DiffFile>>) {
        self.commit_range_diff_results
            .borrow_mut()
            .push_back(result);
    }

    /// A handle to the call counter that stays readable after the mock has been
    /// moved into `App`.
    fn call_counter(&self) -> Arc<AtomicUsize> {
        Arc::clone(&self.working_tree_diff_calls)
    }

    /// A handle to the per-call grammar counts, readable after the same move.
    fn grammar_counts(&self) -> Arc<Mutex<Vec<usize>>> {
        Arc::clone(&self.grammar_counts)
    }

    /// A handle to the per-call commit ids seen by `get_commit_range_diff`,
    /// readable after the mock has been moved into `App`.
    fn commit_range_diff_ids(&self) -> Arc<Mutex<Vec<Vec<String>>>> {
        Arc::clone(&self.commit_range_diff_ids)
    }
}

impl VcsBackend for ScriptedVcs {
    fn info(&self) -> &VcsInfo {
        &self.info
    }

    fn get_working_tree_diff(&self, highlighter: &SyntaxHighlighter) -> Result<Vec<DiffFile>> {
        self.working_tree_diff_calls.fetch_add(1, Ordering::SeqCst);
        self.grammar_counts
            .lock()
            .expect("grammar counts poisoned")
            .push(highlighter.syntax_set.syntaxes().len());
        self.working_tree_diff_results
            .borrow_mut()
            .pop_front()
            .expect("ScriptedVcs: get_working_tree_diff called more times than scripted")
    }

    fn get_commit_range_diff(
        &self,
        revision_range: &ResolvedRevisionRange<'_>,
        _highlighter: &SyntaxHighlighter,
    ) -> Result<Vec<DiffFile>> {
        self.commit_range_diff_ids
            .lock()
            .expect("commit range diff ids poisoned")
            .push(revision_range.commit_ids.to_vec());
        self.commit_range_diff_results
            .borrow_mut()
            .pop_front()
            .expect("ScriptedVcs: get_commit_range_diff called more times than scripted")
    }

    fn fetch_context_lines(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
        _start_line: u32,
        _end_line: u32,
    ) -> Result<Vec<DiffLine>> {
        Ok(Vec::new())
    }

    fn file_line_count(
        &self,
        _file_path: &Path,
        _file_status: FileStatus,
        _ref_commit: Option<&str>,
    ) -> Result<u32> {
        Ok(0)
    }
}

fn test_vcs_info() -> VcsInfo {
    VcsInfo {
        root_path: PathBuf::from("/tmp"),
        head_commit: "abc123".to_string(),
        branch_name: Some("main".to_string()),
        vcs_type: VcsType::Git,
    }
}

fn build_app_with_scripted_vcs(initial_files: Vec<DiffFile>, vcs: ScriptedVcs) -> App {
    let vcs_info = vcs.info().clone();
    let session = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );

    App::build(
        Box::new(vcs),
        vcs_info,
        Theme::dark(),
        None,
        false,
        initial_files,
        session,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("failed to build test app")
}

/// Hunks are left empty: these tests never render content.
fn make_diff_file(path: &str, status: FileStatus, content_hash: u64) -> DiffFile {
    DiffFile {
        old_path: None,
        new_path: Some(PathBuf::from(path)),
        status,
        hunks: Vec::new(),
        is_binary: false,
        is_too_large: false,
        is_commit_message: false,
        content_hash,
    }
}

fn make_commit_info(id: &str) -> CommitInfo {
    CommitInfo {
        id: id.to_string(),
        short_id: id.to_string(),
        branch_name: None,
        summary: format!("commit {id}"),
        body: None,
        author: "tester".to_string(),
        time: chrono::Utc::now(),
    }
}

#[test]
fn should_reload_diff_files_installing_fetched_files() {
    let _reviews = TestReviewsDir::new();
    let old_file = make_diff_file("old.rs", FileStatus::Modified, 1);
    let fetched = vec![
        make_diff_file("a.rs", FileStatus::Added, 10),
        make_diff_file("b.rs", FileStatus::Added, 20),
    ];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(fetched));
    let mut app = build_app_with_scripted_vcs(vec![old_file], vcs);

    let (file_count, invalidated_count) = app.reload_diff_files().expect("reload should succeed");

    assert_eq!(file_count, 2);
    assert_eq!(invalidated_count, 0);
    let paths: std::collections::HashSet<_> = app
        .diff_files
        .iter()
        .map(|f| f.display_path().clone())
        .collect();
    assert_eq!(
        paths,
        [PathBuf::from("a.rs"), PathBuf::from("b.rs")]
            .into_iter()
            .collect()
    );
}

#[test]
fn should_persist_reload_invalidation() {
    let _reviews = TestReviewsDir::new();
    let initial = make_diff_file("a.rs", FileStatus::Modified, 1);
    let changed = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![changed]));
    let mut app = build_app_with_scripted_vcs(vec![initial.clone()], vcs);
    app.session
        .files
        .get_mut(initial.display_path())
        .expect("file should be registered")
        .reviewed = true;
    let session_path = app
        .save_current_session_merging_external()
        .expect("initial session should save");

    let (_, invalidated) = app.reload_diff_files().expect("reload should succeed");

    assert_eq!(invalidated, 1);
    let persisted = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    let review = persisted
        .files
        .get(initial.display_path())
        .expect("file should remain registered");
    assert_eq!(review.content_hash, Some(2));
    assert!(!review.reviewed);
}

#[test]
fn should_persist_startup_reconciliation_without_clearing_new_local_mark() {
    let _reviews = TestReviewsDir::new();
    let old = make_diff_file("a.rs", FileStatus::Modified, 1);
    let current = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![current.clone()]));
    let vcs_info = vcs.info().clone();
    let mut persisted = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    persisted.add_diff_file(&old);
    persisted
        .get_file_mut(old.display_path())
        .expect("file should be registered")
        .reviewed = true;
    crate::persistence::storage::save_session(&persisted).expect("persisted session should save");
    let mut app = App::build(
        Box::new(vcs),
        vcs_info.clone(),
        Theme::dark(),
        None,
        false,
        vec![current],
        persisted,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("app should build");
    app.ensure_ephemeral_session_file()
        .expect("startup reconciliation should persist");
    app.session
        .get_file_mut(old.display_path())
        .expect("file should be registered")
        .reviewed = true;
    app.dirty = true;

    app.reload_diff_files().expect("reload should succeed");

    assert!(
        app.session
            .files
            .get(old.display_path())
            .expect("live file should remain registered")
            .reviewed
    );
    let path = crate::persistence::storage::session_path(&app.session)
        .expect("session path should resolve");
    let saved =
        crate::persistence::storage::load_session(&path).expect("persisted session should load");
    let review = saved
        .files
        .get(old.display_path())
        .expect("file should remain registered");
    assert_eq!(review.content_hash, Some(2));
    assert!(!review.reviewed);
}

#[test]
fn should_merge_external_comment_before_tracking_startup_file_state() {
    let _reviews = TestReviewsDir::new();
    let file = make_diff_file("a.rs", FileStatus::Modified, 1);
    let vcs = ScriptedVcs::new();
    let vcs_info = vcs.info().clone();
    let mut stale = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    stale.add_diff_file(&file);
    let mut latest = stale.clone();
    latest
        .get_file_mut(file.display_path())
        .expect("file should be registered")
        .file_comments
        .push(Comment::new(
            "external".to_string(),
            CommentType::from_id("note"),
            None,
        ));
    crate::persistence::storage::save_session(&latest).expect("external session should save");
    let mut app = App::build(
        Box::new(vcs),
        vcs_info,
        Theme::dark(),
        None,
        false,
        vec![file.clone()],
        stale,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("app should build");

    app.ensure_ephemeral_session_file()
        .expect("session initialization should succeed");

    assert_eq!(
        app.session
            .files
            .get(file.display_path())
            .expect("file should remain registered")
            .file_comments
            .len(),
        1
    );
}

#[test]
fn should_not_save_unrelated_dirty_state_during_reload() {
    let _reviews = TestReviewsDir::new();
    let initial_a = make_diff_file("a.rs", FileStatus::Modified, 1);
    let initial_b = make_diff_file("b.rs", FileStatus::Modified, 1);
    let changed_a = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![changed_a, initial_b.clone()]));
    let mut app = build_app_with_scripted_vcs(vec![initial_a.clone(), initial_b.clone()], vcs);
    let session_path = app
        .save_current_session_merging_external()
        .expect("initial session should save");
    app.session
        .files
        .get_mut(initial_b.display_path())
        .expect("file should be registered")
        .reviewed = true;
    app.dirty = true;

    app.reload_diff_files().expect("reload should succeed");

    assert!(app.dirty);
    let persisted = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    assert_eq!(
        persisted
            .files
            .get(initial_a.display_path())
            .expect("changed file should remain registered")
            .content_hash,
        Some(2)
    );
    assert!(
        !persisted
            .files
            .get(initial_b.display_path())
            .expect("unchanged file should remain registered")
            .reviewed
    );
}

#[test]
fn should_merge_external_comments_without_saving_local_comments() {
    let _reviews = TestReviewsDir::new();
    let initial_a = make_diff_file("a.rs", FileStatus::Modified, 1);
    let initial_b = make_diff_file("b.rs", FileStatus::Modified, 1);
    let changed_a = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![changed_a, initial_b.clone()]));
    let mut app = build_app_with_scripted_vcs(vec![initial_a.clone(), initial_b.clone()], vcs);
    let session_path = app
        .save_current_session_merging_external()
        .expect("initial session should save");
    app.session
        .files
        .get_mut(initial_b.display_path())
        .expect("file should be registered")
        .file_comments
        .push(Comment::new(
            "local".to_string(),
            CommentType::from_id("note"),
            None,
        ));
    app.dirty = true;
    let mut external = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    external
        .files
        .get_mut(initial_a.display_path())
        .expect("file should be registered")
        .file_comments
        .push(Comment::new(
            "external".to_string(),
            CommentType::from_id("note"),
            None,
        ));
    crate::persistence::storage::save_session(&external).expect("external comment should save");

    let (_, _, added_comments) = app
        .reload_diff_files_with_external_comments()
        .expect("reload should succeed");

    assert_eq!(added_comments, 1);
    assert_eq!(
        app.session
            .files
            .get(initial_a.display_path())
            .expect("file should remain registered")
            .file_comments
            .len(),
        1
    );
    assert_eq!(
        app.session
            .files
            .get(initial_b.display_path())
            .expect("file should remain registered")
            .file_comments
            .len(),
        1
    );
    let persisted = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    assert_eq!(
        persisted
            .files
            .get(initial_a.display_path())
            .expect("file should remain registered")
            .file_comments
            .len(),
        1
    );
    assert!(
        persisted
            .files
            .get(initial_b.display_path())
            .expect("file should remain registered")
            .file_comments
            .is_empty()
    );
}

#[test]
fn should_merge_external_comment_for_narrowed_reload_without_persisting_diff() {
    let _reviews = TestReviewsDir::new();
    let initial = make_diff_file("a.rs", FileStatus::Modified, 1);
    let narrowed = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_commit_range_diff(Ok(vec![narrowed]));
    let mut app = build_app_with_scripted_vcs(vec![initial.clone()], vcs);
    app.diff_source = DiffSource::CommitRange(vec!["c1".to_string(), "c2".to_string()]);
    app.review_commits = vec![make_commit_info("c2"), make_commit_info("c1")];
    app.commit_selection_range = Some((0, 0));
    let session_path = app
        .save_current_session_merging_external()
        .expect("initial session should save");
    let mut external = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    external
        .get_file_mut(initial.display_path())
        .expect("file should be registered")
        .file_comments
        .push(Comment::new(
            "external".to_string(),
            CommentType::from_id("note"),
            None,
        ));
    crate::persistence::storage::save_session(&external).expect("external comment should save");

    let (_, _, added_comments) = app
        .reload_diff_files_with_external_comments()
        .expect("reload should succeed");

    assert_eq!(added_comments, 1);
    assert_eq!(
        app.session
            .files
            .get(initial.display_path())
            .expect("file should remain registered")
            .content_hash,
        Some(2)
    );
    let persisted = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    assert_eq!(
        persisted
            .files
            .get(initial.display_path())
            .expect("file should remain registered")
            .content_hash,
        Some(1)
    );
}

#[test]
fn should_preserve_external_review_of_reloaded_content() {
    let _reviews = TestReviewsDir::new();
    let initial = make_diff_file("a.rs", FileStatus::Modified, 1);
    let changed = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![changed]));
    let mut app = build_app_with_scripted_vcs(vec![initial.clone()], vcs);
    app.session
        .files
        .get_mut(initial.display_path())
        .expect("file should be registered")
        .reviewed = true;
    let session_path = app
        .save_current_session_merging_external()
        .expect("initial session should save");
    let mut external = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    let external_review = external
        .files
        .get_mut(initial.display_path())
        .expect("file should be registered");
    external_review.content_hash = Some(2);
    external_review.reviewed = true;
    crate::persistence::storage::save_session(&external).expect("external review should save");
    app.command_buffer = "e".to_string();

    crate::handler::handle_command_action(&mut app, crate::input::Action::SubmitInput);

    assert!(
        app.session
            .files
            .get(initial.display_path())
            .expect("file should remain registered")
            .reviewed
    );
}

#[test]
fn should_preserve_external_review_after_startup_reconciliation() {
    let _reviews = TestReviewsDir::new();
    let old = make_diff_file("a.rs", FileStatus::Modified, 1);
    let current = make_diff_file("a.rs", FileStatus::Modified, 2);
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(vec![current.clone()]));
    let vcs_info = vcs.info().clone();
    let mut persisted = ReviewSession::new(
        vcs_info.root_path.clone(),
        vcs_info.head_commit.clone(),
        vcs_info.branch_name.clone(),
        SessionDiffSource::WorkingTree,
    );
    persisted.add_diff_file(&old);
    let session_path = crate::persistence::storage::save_session(&persisted)
        .expect("persisted session should save");
    let mut app = App::build(
        Box::new(vcs),
        vcs_info,
        Theme::dark(),
        None,
        false,
        vec![current.clone()],
        persisted,
        DiffSource::WorkingTree,
        InputMode::Normal,
        Vec::new(),
        None,
        None,
    )
    .expect("app should build");
    app.ensure_ephemeral_session_file()
        .expect("startup reconciliation should persist");
    let mut external = crate::persistence::storage::load_session(&session_path)
        .expect("persisted session should load");
    external
        .get_file_mut(current.display_path())
        .expect("file should be registered")
        .reviewed = true;
    crate::persistence::storage::save_session(&external).expect("external review should save");

    app.reload_diff_files().expect("reload should succeed");

    assert!(
        app.session
            .files
            .get(current.display_path())
            .expect("file should remain registered")
            .reviewed
    );
}

#[test]
fn should_return_none_when_fetched_diff_is_unchanged() {
    let files = vec![
        make_diff_file("a.rs", FileStatus::Modified, 1),
        make_diff_file("b.rs", FileStatus::Modified, 2),
    ];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(files.clone()));
    let app = build_app_with_scripted_vcs(files, vcs);

    let result = app.fetch_changed_diff_files();

    assert!(
        matches!(result, Ok(None)),
        "expected Ok(None), got {result:?}"
    );
}

#[test]
fn should_return_fetched_files_when_diff_changed() {
    let initial = vec![make_diff_file("a.rs", FileStatus::Modified, 1)];
    let changed = vec![make_diff_file("a.rs", FileStatus::Modified, 2)];
    let vcs = ScriptedVcs::new();
    // The probe and the real fetch each consume one scripted response.
    vcs.push_working_tree_diff(Ok(changed.clone()));
    vcs.push_working_tree_diff(Ok(changed));
    let app = build_app_with_scripted_vcs(initial, vcs);

    let result = app
        .fetch_changed_diff_files()
        .expect("fetch should succeed");
    let files = result.expect("expected Some(files) since content_hash changed");

    assert_eq!(files.len(), 1);
    assert_eq!(files[0].display_path(), &PathBuf::from("a.rs"));
    assert_eq!(files[0].content_hash, 2);
}

/// An unchanged tick must cost exactly one backend fetch, the cheap probe. A
/// second fetch means the expensive path ran anyway and the gate saved nothing.
#[test]
fn should_fetch_once_when_probe_finds_no_change() {
    let files = vec![
        make_diff_file("a.rs", FileStatus::Modified, 1),
        make_diff_file("b.rs", FileStatus::Modified, 2),
    ];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(files.clone()));
    let counter = vcs.call_counter();
    let app = build_app_with_scripted_vcs(files, vcs);

    let result = app.fetch_changed_diff_files();

    assert!(
        matches!(result, Ok(None)),
        "expected Ok(None), got {result:?}"
    );
    assert_eq!(
        counter.load(Ordering::SeqCst),
        1,
        "unchanged tick must cost one cheap probe, not a second full fetch"
    );
}

/// A change must cost two fetches: the cheap probe that detects it, then the
/// highlighted fetch that gets applied.
#[test]
fn should_fetch_twice_when_probe_finds_a_change() {
    let initial = vec![make_diff_file("a.rs", FileStatus::Modified, 1)];
    let changed = vec![make_diff_file("a.rs", FileStatus::Modified, 2)];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(changed.clone()));
    vcs.push_working_tree_diff(Ok(changed));
    let counter = vcs.call_counter();
    let app = build_app_with_scripted_vcs(initial, vcs);

    let files = app
        .fetch_changed_diff_files()
        .expect("fetch should succeed")
        .expect("expected Some(files) since content_hash changed");

    assert_eq!(files[0].content_hash, 2);
    assert_eq!(
        counter.load(Ordering::SeqCst),
        2,
        "a changed tick pays probe plus real fetch"
    );
}

/// The probe only saves anything if it runs against a highlighter with no
/// grammars. Swapping it for the real one leaves every other test in this file
/// passing, because the call counts and the returned diff are unchanged, while
/// the gate quietly costs a full highlight pass on every tick.
#[test]
fn should_probe_with_a_grammar_free_highlighter_then_fetch_with_the_real_one() {
    let initial = vec![make_diff_file("a.rs", FileStatus::Modified, 1)];
    let changed = vec![make_diff_file("a.rs", FileStatus::Modified, 2)];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(changed.clone()));
    vcs.push_working_tree_diff(Ok(changed));
    let grammars = vcs.grammar_counts();
    let app = build_app_with_scripted_vcs(initial, vcs);

    app.fetch_changed_diff_files()
        .expect("fetch should succeed")
        .expect("expected Some(files) since content_hash changed");

    let counts = grammars.lock().expect("grammar counts poisoned");
    assert_eq!(counts.len(), 2, "expected a probe fetch and a real fetch");
    assert_eq!(counts[0], 0, "the probe must load no grammars");
    assert!(
        counts[1] > 0,
        "the applied fetch must use the real highlighter, got {} grammars",
        counts[1]
    );
}

/// `src/app/reviewed.rs:36` matches on `Err(TuicrError::NoChanges)` from
/// `reload_diff_files` to clear the diff view after staging empties it out;
/// the split must keep propagating that error through `?`.
#[test]
fn should_propagate_no_changes_when_backend_reports_no_changes() {
    let initial = vec![make_diff_file("a.rs", FileStatus::Modified, 1)];
    let vcs = ScriptedVcs::new();
    vcs.push_working_tree_diff(Ok(Vec::new()));
    let mut app = build_app_with_scripted_vcs(initial, vcs);

    let result = app.reload_diff_files();

    assert!(
        matches!(result, Err(TuicrError::NoChanges)),
        "expected Err(NoChanges), got {result:?}"
    );
}

/// Regression test: narrow the inline commit selector to a subset of commits,
/// then trigger a generic reload (`:e`, editor exit both route through
/// `reload_diff_files`). The reload must keep re-fetching the narrowed
/// subrange, not silently widen back out to every commit in the review while
/// the selector still shows the subset. Before the fix,
/// `fetch_diff_files_for_source` matched purely on `self.diff_source`'s full
/// commit list and never consulted `commit_selection_range`.
#[test]
fn should_reload_diff_files_keeping_narrowed_commit_selection() {
    let narrowed = vec![make_diff_file("c3.rs", FileStatus::Modified, 30)];
    let vcs = ScriptedVcs::new();
    vcs.push_commit_range_diff(Ok(narrowed.clone()));
    let commit_ids_seen = vcs.commit_range_diff_ids();
    let mut app = build_app_with_scripted_vcs(narrowed, vcs);
    app.diff_source =
        DiffSource::CommitRange(vec!["c1".to_string(), "c2".to_string(), "c3".to_string()]);
    // `review_commits` is always stored newest-first; narrowed to just the
    // newest commit (c3), matching the initially loaded `diff_files` above.
    app.review_commits = vec![
        make_commit_info("c3"),
        make_commit_info("c2"),
        make_commit_info("c1"),
    ];
    app.commit_selection_range = Some((0, 0));

    app.reload_diff_files().expect("reload should succeed");

    assert_eq!(
        commit_ids_seen
            .lock()
            .expect("commit ids poisoned")
            .as_slice(),
        [vec!["c3".to_string()]],
        "reload must fetch only the narrowed selection, not the full commit range"
    );
}

/// Same narrowing bug, exercised through the diff-watch shared gate
/// (`changed_diff_files_for_source`) instead of `reload_diff_files`.
/// `diff_watch_fetch` runs this gate on a worker thread from a snapshot with
/// no `App` access, so the fix has to live in the gate itself, not only in
/// the `&self` reload path this test's sibling covers.
#[test]
fn should_fetch_changed_diff_files_keeping_narrowed_commit_selection() {
    let narrowed = vec![make_diff_file("c3.rs", FileStatus::Modified, 30)];
    let vcs = ScriptedVcs::new();
    vcs.push_commit_range_diff(Ok(narrowed.clone()));
    let commit_ids_seen = vcs.commit_range_diff_ids();
    let mut app = build_app_with_scripted_vcs(narrowed, vcs);
    app.diff_source =
        DiffSource::CommitRange(vec!["c1".to_string(), "c2".to_string(), "c3".to_string()]);
    app.review_commits = vec![
        make_commit_info("c3"),
        make_commit_info("c2"),
        make_commit_info("c1"),
    ];
    app.commit_selection_range = Some((0, 0));

    app.fetch_changed_diff_files()
        .expect("fetch should succeed");

    assert_eq!(
        commit_ids_seen
            .lock()
            .expect("commit ids poisoned")
            .as_slice(),
        [vec!["c3".to_string()]],
        "diff-watch's probe fetch must use the narrowed selection, not the full commit range"
    );
}
