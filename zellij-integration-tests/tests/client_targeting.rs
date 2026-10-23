#![cfg(unix)]

use std::collections::BTreeMap;
use std::time::{Duration, Instant};
use zellij_integration_tests::{FakePtyHandle, TestRunner, TestSession, TERMINAL_SIZE};
use zellij_utils::cli::CliAction;
use zellij_utils::data::LayoutInfo;

fn runner() -> TestRunner {
    TestRunner::new(TERMINAL_SIZE)
        .with_config("mirror_session false")
        .with_layout(LayoutInfo::Stringified("layout { pane; pane; }".to_owned()))
}

fn start() -> (TestSession, FakePtyHandle, FakePtyHandle) {
    let session = runner().start();
    let first = session.expect_pty_spawn();
    let second = session.expect_pty_spawn();
    first.output(b"first pane $ ");
    second.output(b"second pane $ ");
    session.wait_until("both cold panes rendered", |grid| {
        grid.contains("first pane") && grid.contains("second pane")
    });
    (session, first, second)
}

fn clients(session: &TestSession) -> BTreeMap<u32, String> {
    let result = session.run_cli_action_with_target(CliAction::ListClients, None);
    assert_eq!(result.exit_code, 0);
    assert!(result.stdout.starts_with("CLIENT_ID"), "{:?}", result);
    result
        .stdout
        .lines()
        .skip(1)
        .map(|line| {
            let mut columns = line.split_whitespace();
            (
                columns.next().unwrap().parse().unwrap(),
                columns.next().unwrap().to_owned(),
            )
        })
        .collect()
}

fn wait_clients(session: &TestSession, count: usize) -> BTreeMap<u32, String> {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = clients(session);
        if snapshot.len() == count {
            return snapshot;
        }
        assert!(
            Instant::now() < deadline,
            "expected {count} clients, got {snapshot:?}"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn focus(session: &TestSession, pane_id: u32, client_id: Option<u32>) -> i32 {
    session
        .run_cli_action_with_target(
            CliAction::FocusPaneId {
                pane_id: format!("terminal_{pane_id}"),
            },
            client_id,
        )
        .exit_code
}

fn switch(session: &TestSession, name: &str, client_id: Option<u32>) -> i32 {
    session
        .run_cli_action_with_target(
            CliAction::SwitchSession {
                name: name.to_owned(),
                tab_position: None,
                pane_id: None,
                layout: None,
                layout_string: None,
                layout_dir: None,
                cwd: None,
                close_current: false,
            },
            client_id,
        )
        .exit_code
}

#[test]
fn cold_focus_changes_the_attached_clients_pane() {
    let (mut session, first, second) = start();
    let before = wait_clients(&session, 1);
    let client_id = *before.keys().next().unwrap();
    let target = if before[&client_id] == format!("terminal_{}", first.terminal_id()) {
        second.terminal_id()
    } else {
        first.terminal_id()
    };
    assert_eq!(focus(&session, target, None), 0);
    let expected = BTreeMap::from([(client_id, format!("terminal_{target}"))]);
    assert_eq!(clients(&session), expected);
    assert_ne!(focus(&session, target, None), 0); // Existing already-focused contract.
    assert_ne!(focus(&session, u32::MAX, None), 0);
    assert_ne!(focus(&session, first.terminal_id(), Some(u32::MAX)), 0);
    assert_ne!(
        session
            .run_cli_action_with_target(
                CliAction::SwitchSession {
                    name: "must-not-be-created".to_owned(),
                    tab_position: None,
                    pane_id: None,
                    layout: None,
                    layout_string: None,
                    layout_dir: None,
                    cwd: None,
                    close_current: true,
                },
                Some(u32::MAX)
            )
            .exit_code,
        0
    );
    assert_eq!(clients(&session), expected);
    session.kill_session();
}

#[test]
fn multiple_cold_clients_require_an_explicit_target() {
    let background = runner().skip_concurrency_slot().start_in_background();
    background
        .expect_pty_spawn()
        .output(b"destination first $ ");
    background
        .expect_pty_spawn()
        .output(b"destination second $ ");
    let (mut session, first, second) = start();
    let first_client = *wait_clients(&session, 1).keys().next().unwrap();
    let other = session.attach_client(TERMINAL_SIZE);
    other.wait_until("second client rendered", |grid| grid.contains("first pane"));
    let before = wait_clients(&session, 2);
    let target = if before[&first_client] == format!("terminal_{}", first.terminal_id()) {
        second.terminal_id()
    } else {
        first.terminal_id()
    };
    assert_ne!(focus(&session, target, None), 0);
    assert_ne!(switch(&session, "must-not-be-created", None), 0);
    assert_eq!(clients(&session), before);
    assert_eq!(focus(&session, target, Some(first_client)), 0);
    let mut expected = before;
    expected.insert(first_client, format!("terminal_{target}"));
    assert_eq!(clients(&session), expected);
    assert_ne!(focus(&session, target, Some(u32::MAX)), 0);
    assert_eq!(clients(&session), expected);
    assert_eq!(
        session
            .run_cli_action_with_target(
                CliAction::NewTab {
                    name: Some("background".to_owned()),
                    layout: None,
                    layout_string: Some("layout { pane; }".to_owned()),
                    layout_dir: None,
                    cwd: None,
                    initial_command: vec![],
                    initial_plugin: None,
                    close_on_exit: false,
                    start_suspended: false,
                    block_until_exit_success: false,
                    block_until_exit_failure: false,
                    block_until_exit: false,
                    no_focus: true,
                },
                None
            )
            .exit_code,
        0
    );
    assert_eq!(clients(&session), expected);

    assert_eq!(
        switch(&session, background.session_name(), Some(first_client)),
        0
    );
    expected.remove(&first_client);
    assert_eq!(wait_clients(&session, 1), expected);
    assert!(!other.has_exited());
    let (_, previous) = session.follow_switch_into(background);
    session.wait_until("selected client reached destination", |grid| {
        grid.contains("destination first")
    });
    wait_clients(&session, 1);
    session.kill_session();
    previous.attach(TERMINAL_SIZE).kill_session();
    other.wait_for_client_to_exit();
}

#[test]
fn cold_cli_switch_round_trip_moves_the_real_client_and_rejects_stale_ids() {
    let background = runner().skip_concurrency_slot().start_in_background();
    let target_first = background.expect_pty_spawn();
    let target_second = background.expect_pty_spawn();
    target_first.output(b"destination first $ ");
    target_second.output(b"destination second $ ");
    let (mut session, _, _) = start();
    let original_name = session.session_name().to_owned();
    let original_clients = wait_clients(&session, 1);
    let original_id = *original_clients.keys().next().unwrap();
    let destination = background.session_name().to_owned();

    assert_eq!(switch(&session, &destination, None), 0);
    wait_clients(&session, 0);
    assert_ne!(switch(&session, &destination, None), 0);
    assert_ne!(focus(&session, 0, Some(original_id)), 0);
    let (_, previous) = session.follow_switch_into(background);
    session.wait_until("client rendered destination", |grid| {
        grid.contains("destination first")
    });
    let destination_clients = wait_clients(&session, 1);
    let destination_id = *destination_clients.keys().next().unwrap();
    assert_eq!(session.session_name(), destination);
    assert_eq!(switch(&session, &original_name, Some(destination_id)), 0);
    wait_clients(&session, 0);
    let (_, destination_background) = session.follow_switch_into(previous);
    session.wait_until("client rendered original session again", |grid| {
        grid.contains("first pane")
    });
    let returned = wait_clients(&session, 1);
    assert_eq!(session.session_name(), original_name);
    assert_ne!(*returned.keys().next().unwrap(), original_id);
    assert_eq!(returned.values().next(), original_clients.values().next());
    assert_ne!(switch(&session, &destination, Some(original_id)), 0);
    assert_eq!(clients(&session), returned);
    session.kill_session();
    destination_background.attach(TERMINAL_SIZE).kill_session();
}
