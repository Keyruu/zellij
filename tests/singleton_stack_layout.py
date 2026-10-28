#!/usr/bin/env python3
"""Run with: python3 tests/singleton_stack_layout.py target/debug/zellij

Owns and deletes only its uniquely named temporary sessions. No real editor or agent runs.
"""
import fcntl
import json
import os
from pathlib import Path
import pty
import re
import select
import struct
import subprocess
import sys
import tempfile
import termios
import threading
import time

BINARY = str(Path(sys.argv[1] if len(sys.argv) > 1 else 'target/debug/zellij').resolve())
ENV = {k: v for k, v in os.environ.items() if not k.startswith(('ZELLIJ', 'PI_'))}
ENV['TERM'] = 'xterm-256color'
LAYOUT = '''layout {
    default_tab_template {
        pane size=1 borderless=true { plugin location="zellij:tab-bar"; }
        children
        pane size=1 borderless=true { plugin location="zellij:status-bar"; }
    }
    tab hide_floating_panes=true {
        pane split_direction="vertical" {
            pane {
                pane command="sh" { args "-c" "printf EDITOR; exec sleep 601"; }
                pane stacked=true stack_direction="horizontal" size="30%" { pane; }
            }
            pane stacked=true stack_direction="vertical" size="40%" {
                pane command="sh" { args "-c" "printf AGENT; exec sleep 602"; }
            }
        }
    }
    swap_floating_layout {
        floating_panes {
            pane { width "80%"; height "80%"; x "10%"; y "10%"; }
        }
    }
}
'''


def cli(*args):
    return subprocess.run([BINARY, *args], env=ENV, capture_output=True, text=True, timeout=12)


def panes_in(session, expected_count=3):
    deadline = time.monotonic() + 15
    while time.monotonic() < deadline:
        result = cli('--session', session, 'action', 'list-panes', '--json', '--all')
        if result.returncode == 0:
            try:
                panes = json.loads(result.stdout)
                if len([p for p in panes if not p['is_plugin']]) == expected_count:
                    return panes
            except json.JSONDecodeError:
                pass
        time.sleep(.1)
    raise AssertionError(f'{session}: panes did not start: {result.stderr}')


def check(session):
    expected = {
        'EDITOR': ('sleep 601', (0, 1, 72, 27)),
        'AGENT': ('sleep 602', (72, 1, 48, 38)),
        'BOTTOM SHELL': ('sleep 603', (0, 28, 72, 11)),
    }
    deadline = time.monotonic() + 10
    while time.monotonic() < deadline:
        found = {}
        for pane in panes_in(session):
            if pane['is_plugin']:
                continue
            screen = cli('--session', session, 'action', 'dump-screen',
                         '--pane-id', f"terminal_{pane['id']}").stdout
            for marker in expected:
                if marker in screen:
                    found[marker] = pane
        if len(found) == 3:
            break
        time.sleep(.1)
    assert len(found) == 3, (session, found)
    for marker, (command, geom) in expected.items():
        pane = found[marker]
        assert pane['pane_command'] == command, (session, marker, pane)
        actual = tuple(pane[f'pane_{field}'] for field in ('x', 'y', 'columns', 'rows'))
        assert actual == geom, (session, marker, actual, geom)


def no_extra_border(output):
    # A singleton stack used to draw a second dashed title line at row 29.
    ansi = output.decode('utf-8', 'replace')
    for match in re.finditer(r'\x1b\[29;1H(.*?)(?=\x1b\[\d+;\d+H|\Z)', ansi, re.S):
        row = re.sub(r'\x1b(?:\[[0-?]*[ -/]*[@-~]|\][^\x07]*(?:\x07|\x1b\\))', '', match[1])
        assert '──' not in row, f'extra stacked title border: {row!r}'
    assert '\x1b[29;1H' in ansi, 'bottom title row was not rendered'


with tempfile.TemporaryDirectory(prefix='zellij-singleton-layout-') as directory:
    root = Path(directory)
    shell = root / 'shell'
    shell.write_text('#!/bin/sh\nprintf "BOTTOM SHELL\\n"\nexec sleep 603\n')
    shell.chmod(0o755)
    config = root / 'config.kdl'
    config.write_text(f'default_shell "{shell}"\npane_frame_style "titles"\n')
    layout = root / 'project.kdl'
    layout.write_text(LAYOUT)
    origin_layout = root / 'origin.kdl'
    origin_layout.write_text('layout { pane; }\n')
    for via_switch in (False, True):
        for attempt in range(3):
            name = f'zellij-singleton-test-{os.getpid()}-{int(via_switch)}-{attempt}'
            origin = name + '-origin'
            owned = [origin, name] if via_switch else [name]
            master, slave = pty.openpty()
            fcntl.ioctl(slave, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
            proc = subprocess.Popen(
                [BINARY, '--config', str(config), '--session', origin if via_switch else name,
                 '--new-session-with-layout', str(origin_layout if via_switch else layout)],
                env=ENV, stdin=slave, stdout=slave, stderr=slave, start_new_session=True,
            )
            os.close(slave)
            output = bytearray()
            stop = threading.Event()

            def drain():
                while not stop.is_set():
                    if select.select([master], [], [], .1)[0]:
                        try:
                            output.extend(os.read(master, 65536))
                        except OSError:
                            break

            reader = threading.Thread(target=drain)
            reader.start()
            try:
                panes_in(origin if via_switch else name, 1 if via_switch else 3)
                if via_switch:
                    result = cli('--session', origin, 'action', 'switch-session',
                                 '--cwd', str(root), '--layout', str(layout), '--', name)
                    assert result.returncode == 0, result.stderr
                check(name)
                # Focus and resize can reapply the layout and previously swapped the commands.
                for action in ('focus-next-pane', 'focus-previous-pane'):
                    result = cli('--session', name, 'action', action)
                    assert result.returncode == 0, result.stderr
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 41, 120, 0, 0))
                time.sleep(.2)
                fcntl.ioctl(master, termios.TIOCSWINSZ, struct.pack('HHHH', 40, 120, 0, 0))
                check(name)
                time.sleep(.3)
                no_extra_border(output)
                print(f'PASS {"switch" if via_switch else "direct"} #{attempt + 1}', flush=True)
            except Exception:
                print(output.decode('utf-8', 'replace')[-3000:], file=sys.stderr)
                raise
            finally:
                for session in owned:
                    cli('delete-session', '--force', '--', session)
                try:
                    proc.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    proc.terminate()
                    proc.wait(timeout=5)
                stop.set()
                reader.join()
                os.close(master)
