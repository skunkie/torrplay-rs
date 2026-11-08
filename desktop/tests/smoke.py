#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Drives a built desktop application on a virtual display.

Run it as `dbus-run-session -- python3 tests/smoke.py PATH_TO_BINARY`. It uses a
private home, a stand-in for `xdg-open`, and invented data, so nothing reaches the
desktop it runs on. It closes windows with the message a window manager sends, since
WebDriver ending its session kills the application instead of closing it. It needs
Xvfb, WebKitWebDriver, libX11, libxdo, and PyGObject with GTK 3, which drags a file
from a window of its own.
"""

import ctypes
import ctypes.util
import glob
import hashlib
import json
import os
import re
import shutil
import signal
import socket
import stat
import subprocess
import sys
import tempfile
import time
import unittest
import urllib.error
import urllib.request

BINARY = ""
DISPLAY = ":99"
PORT = 18642
ORIGIN = f"http://127.0.0.1:{PORT}"
DRIVER_PORT = 14444
WATCH = "/demo/watch/trr_aaaaaaaaaaaaaaaaaaaaaaaaa1/fil_aaaaaaaaaaaaaaaaaaaaaa1003"
READY_SECONDS = 60
# Longer than the application's own limit on shutting down.
STOP_SECONDS = 50


class XEvent(ctypes.Structure):
    _fields_ = [
        ("type", ctypes.c_int),
        ("serial", ctypes.c_ulong),
        ("send_event", ctypes.c_int),
        ("display", ctypes.c_void_p),
        ("window", ctypes.c_ulong),
        ("message_type", ctypes.c_ulong),
        ("format", ctypes.c_int),
        ("data", ctypes.c_long * 5),
        ("padding", ctypes.c_long * 12),
    ]


class Windows:
    """The top-level windows on the virtual display, through libX11."""

    CLIENT_MESSAGE = 33

    def __init__(self):
        x11 = ctypes.CDLL(ctypes.util.find_library("X11"))
        x11.XOpenDisplay.restype = ctypes.c_void_p
        x11.XOpenDisplay.argtypes = [ctypes.c_char_p]
        x11.XDefaultRootWindow.restype = ctypes.c_ulong
        x11.XDefaultRootWindow.argtypes = [ctypes.c_void_p]
        x11.XInternAtom.restype = ctypes.c_ulong
        x11.XInternAtom.argtypes = [ctypes.c_void_p, ctypes.c_char_p, ctypes.c_int]
        window_list = ctypes.POINTER(ctypes.POINTER(ctypes.c_ulong))
        x11.XQueryTree.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.POINTER(ctypes.c_ulong),
            ctypes.POINTER(ctypes.c_ulong),
            window_list,
            ctypes.POINTER(ctypes.c_uint),
        ]
        x11.XGetWMProtocols.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            window_list,
            ctypes.POINTER(ctypes.c_int),
        ]
        x11.XSendEvent.argtypes = [
            ctypes.c_void_p,
            ctypes.c_ulong,
            ctypes.c_int,
            ctypes.c_long,
            ctypes.POINTER(XEvent),
        ]
        x11.XSync.argtypes = [ctypes.c_void_p, ctypes.c_int]
        # A window can go between listing it and asking about it, and Xlib's own
        # handler then ends the process; such a window is simply not listed.
        handler = ctypes.CFUNCTYPE(ctypes.c_int, ctypes.c_void_p, ctypes.c_void_p)
        x11.XSetErrorHandler.argtypes = [handler]
        x11.XSetErrorHandler.restype = ctypes.c_void_p
        self._ignore_errors = handler(lambda _display, _event: 0)
        x11.XSetErrorHandler(self._ignore_errors)
        self.x11 = x11
        self.display = x11.XOpenDisplay(DISPLAY.encode())
        self.protocols = x11.XInternAtom(self.display, b"WM_PROTOCOLS", 0)
        self.delete = x11.XInternAtom(self.display, b"WM_DELETE_WINDOW", 0)

    def _children(self, window):
        root = ctypes.c_ulong()
        parent = ctypes.c_ulong()
        children = ctypes.POINTER(ctypes.c_ulong)()
        count = ctypes.c_uint()
        if not self.x11.XQueryTree(
            self.display,
            window,
            ctypes.byref(root),
            ctypes.byref(parent),
            ctypes.byref(children),
            ctypes.byref(count),
        ):
            return []
        found = []
        for index in range(count.value):
            found.append(children[index])
            found.extend(self._children(children[index]))
        return found

    def closable(self):
        """The windows that take the close message, a window's helpers among them."""
        self.x11.XSync(self.display, 0)
        found = []
        for window in self._children(self.x11.XDefaultRootWindow(self.display)):
            protocols = ctypes.POINTER(ctypes.c_ulong)()
            count = ctypes.c_int()
            if self.x11.XGetWMProtocols(
                self.display, window, ctypes.byref(protocols), ctypes.byref(count)
            ) and any(protocols[index] == self.delete for index in range(count.value)):
                found.append(window)
        return found

    def close_all(self):
        for window in self.closable():
            event = XEvent(
                type=self.CLIENT_MESSAGE,
                window=window,
                message_type=self.protocols,
                format=32,
            )
            event.data[0] = self.delete
            self.x11.XSendEvent(self.display, window, 0, 0, ctypes.byref(event))
        self.x11.XSync(self.display, 0)


# The other side of a real file drop: a small window that offers a file by dragging.
DRAG_SOURCE = """
import sys

import gi

gi.require_version("Gdk", "3.0")
gi.require_version("Gtk", "3.0")
from gi.repository import Gdk, Gtk

uri = sys.argv[1]
window = Gtk.Window(type=Gtk.WindowType.POPUP)
window.move(1000, 600)
window.set_default_size(200, 150)
box = Gtk.EventBox()
box.drag_source_set(
    Gdk.ModifierType.BUTTON1_MASK,
    [Gtk.TargetEntry.new("text/uri-list", 0, 0)],
    Gdk.DragAction.COPY,
)
box.connect("drag-data-get", lambda _w, _c, data, _i, _t: data.set_uris([uri]))
window.add(box)
window.show_all()
print("ready", flush=True)
Gtk.main()
"""


def bencode(value):
    if isinstance(value, int):
        return b"i%de" % value
    if isinstance(value, str):
        value = value.encode()
    if isinstance(value, bytes):
        return b"%d:%s" % (len(value), value)
    return (
        b"d"
        + b"".join(bencode(key) + bencode(value[key]) for key in sorted(value))
        + b"e"
    )


def write_torrent(directory, name):
    """Writes a torrent file of a file called `name`, and returns its path."""
    info = {
        "name": name,
        "length": 16384,
        "piece length": 16384,
        "pieces": hashlib.sha1(name.encode()).digest(),
    }
    path = os.path.join(directory, f"{name}.torrent")
    with open(path, "wb") as file:
        file.write(bencode({"announce": "http://127.0.0.1:1/announce", "info": info}))
    return path


def drag(start, end):
    """Presses the mouse at `start`, moves in steps, and lets go at `end`."""
    xdo_library = ctypes.CDLL("libxdo.so.3")
    xdo_library.xdo_new.restype = ctypes.c_void_p
    xdo_library.xdo_new.argtypes = [ctypes.c_char_p]
    xdo_library.xdo_move_mouse.argtypes = [
        ctypes.c_void_p,
        ctypes.c_int,
        ctypes.c_int,
        ctypes.c_int,
    ]
    xdo_library.xdo_mouse_down.argtypes = [
        ctypes.c_void_p,
        ctypes.c_ulong,
        ctypes.c_int,
    ]
    xdo_library.xdo_mouse_up.argtypes = [ctypes.c_void_p, ctypes.c_ulong, ctypes.c_int]
    xdo = xdo_library.xdo_new(DISPLAY.encode())
    xdo_library.xdo_move_mouse(xdo, *start, 0)
    time.sleep(0.3)
    xdo_library.xdo_mouse_down(xdo, 0, 1)
    time.sleep(0.2)
    steps = 20
    for step in range(1, steps + 1):
        xdo_library.xdo_move_mouse(
            xdo,
            start[0] + (end[0] - start[0]) * step // steps,
            start[1] + (end[1] - start[1]) * step // steps,
            0,
        )
        time.sleep(0.05)
    time.sleep(0.5)
    xdo_library.xdo_mouse_up(xdo, 0, 1)


def wait_until(check, seconds, what):
    deadline = time.monotonic() + seconds
    while time.monotonic() < deadline:
        value = check()
        if value:
            return value
        time.sleep(0.2)
    raise AssertionError(f"timed out waiting for {what}")


def listening():
    with socket.socket() as probe:
        return probe.connect_ex(("127.0.0.1", PORT)) == 0


class Environment:
    """A private home and data directory, and the environment to run in them."""

    def __init__(self):
        self.root = tempfile.mkdtemp(prefix="torrplay-desktop-smoke-")
        self.home = os.path.join(self.root, "home")
        self.data = os.path.join(self.root, "data")
        self.opened = os.path.join(self.root, "opened.log")
        stubs = os.path.join(self.root, "bin")
        os.makedirs(self.home)
        os.makedirs(stubs)
        stub = os.path.join(stubs, "xdg-open")
        with open(stub, "w", encoding="utf-8") as file:
            file.write(f'#!/bin/sh\necho "$@" >> "{self.opened}"\n')
        os.chmod(stub, stat.S_IRWXU)
        self.variables = {
            **os.environ,
            "DISPLAY": DISPLAY,
            "GDK_BACKEND": "x11",
            "HOME": self.home,
            "XDG_CONFIG_HOME": os.path.join(self.home, ".config"),
            "XDG_DATA_HOME": os.path.join(self.home, ".local", "share"),
            "XDG_CACHE_HOME": os.path.join(self.home, ".cache"),
            "PATH": stubs + os.pathsep + os.environ["PATH"],
        }
        self.variables.pop("WAYLAND_DISPLAY", None)
        self.arguments = [
            "--data-dir",
            self.data,
            "--listen",
            f"127.0.0.1:{PORT}",
            "--peer-port",
            "27913",
            "--no-dht",
        ]

    def start(self, extra_arguments=()):
        return subprocess.Popen(
            [BINARY, *self.arguments, *extra_arguments],
            env=self.variables,
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )

    def log_messages(self):
        path = os.path.join(self.data, "logs", "torrplay.jsonl")
        with open(path, encoding="utf-8") as file:
            return [json.loads(line)["message"] for line in file]

    def opened_links(self):
        if not os.path.exists(self.opened):
            return []
        with open(self.opened, encoding="utf-8") as file:
            return file.read().splitlines()

    def downloads(self):
        return sorted(
            os.path.basename(path)
            for path in glob.glob(os.path.join(self.home, "Downloads", "*"))
        )

    def kill_processes(self):
        """Ends what this environment started, and no other copy of the application:
        only its processes name its data directory on their command lines. Waits until
        they are gone, not just the port, since one that still runs would take the next
        launch for a second one. Outright, since these tests' data does not matter."""
        pattern = re.escape(self.data)
        subprocess.run(["pkill", "-KILL", "-f", pattern], check=False)
        wait_until(
            lambda: subprocess.run(
                ["pgrep", "-f", pattern], capture_output=True, check=False
            ).returncode
            != 0,
            20,
            "the application to end",
        )

    def remove(self):
        shutil.rmtree(self.root, ignore_errors=True)


class Driver:
    """A WebDriver session on the application in its window."""

    def __init__(self, environment, extra_arguments=()):
        self.process = subprocess.Popen(
            ["WebKitWebDriver", f"--port={DRIVER_PORT}"],
            env={
                **environment.variables,
                "TAURI_AUTOMATION": "true",
                "TAURI_WEBVIEW_AUTOMATION": "true",
            },
            stdout=subprocess.DEVNULL,
            stderr=subprocess.DEVNULL,
        )
        wait_until(self._driver_up, 20, "WebKitWebDriver")
        self.environment = environment
        created = self._call(
            "POST",
            "/session",
            {
                "capabilities": {
                    "alwaysMatch": {
                        "browserName": "wry",
                        "webkitgtk:browserOptions": {
                            "binary": os.path.abspath(BINARY),
                            "args": [*environment.arguments, *extra_arguments],
                        },
                    }
                }
            },
            READY_SECONDS,
        )
        self.session = f"/session/{created['sessionId']}"

    def _driver_up(self):
        with socket.socket() as probe:
            return probe.connect_ex(("127.0.0.1", DRIVER_PORT)) == 0

    def _call(self, method, path, body=None, timeout=30):
        request = urllib.request.Request(
            f"http://127.0.0.1:{DRIVER_PORT}{path}",
            method=method,
            data=None if body is None else json.dumps(body).encode(),
            headers={"Content-Type": "application/json"},
        )
        try:
            with urllib.request.urlopen(request, timeout=timeout) as response:
                return json.load(response)["value"]
        except urllib.error.HTTPError as error:
            raise AssertionError(f"{path}: {error.read().decode()[:300]}") from error

    def script(self, code, *arguments):
        return self._call(
            "POST",
            f"{self.session}/execute/sync",
            {"script": code, "args": list(arguments)},
        )

    def wait_for_add_page(self, text):
        wait_until(
            lambda: self.script(
                "return location.pathname === '/add'"
                " && document.body.innerText.includes(arguments[0])",
                text,
            ),
            30,
            f"the Add page to show {text}",
        )

    def go(self, path):
        self._call("POST", f"{self.session}/url", {"url": ORIGIN + path})

    def click(self, xpath):
        element = wait_until(
            lambda: self._find(xpath), 20, f"an element at {xpath}"
        )
        self._call("POST", f"{self.session}/element/{element}/click", {})

    def _find(self, xpath):
        found = self._call(
            "POST",
            f"{self.session}/elements",
            {"using": "xpath", "value": xpath},
        )
        return next(iter(found[0].values())) if found else None

    def stop(self):
        self.process.send_signal(signal.SIGTERM)
        self.process.wait(10)
        self.environment.kill_processes()
        wait_until(lambda: not listening(), STOP_SECONDS, "the port to be free")


class WindowTests(unittest.TestCase):
    """What the page does in the window, in one session, since starting one is slow."""

    @classmethod
    def setUpClass(cls):
        cls.environment = Environment()
        cls.driver = Driver(cls.environment)

    @classmethod
    def tearDownClass(cls):
        cls.driver.stop()
        cls.environment.remove()

    def test_opens_on_its_own_address(self):
        self.driver.go("/")
        self.assertEqual(self.driver.script("return location.origin"), ORIGIN)
        self.assertEqual(self.driver.script("return document.title"), "TorrPlay")

    def test_plays_a_video(self):
        self.driver.go(WATCH)
        playing = wait_until(
            lambda: self.driver.script(
                "const video = document.querySelector('video');"
                "return video && !video.error && video.currentTime > 0"
                " ? video.currentTime : null"
            ),
            READY_SECONDS,
            "the video to play",
        )
        wait_until(
            lambda: self.driver.script(
                "return document.querySelector('video').currentTime"
            )
            > playing,
            20,
            "the video to move on",
        )

    def test_saves_downloads_under_new_names(self):
        self.driver.go("/settings/backup")
        for count in (1, 2):
            self.driver.click("//button[normalize-space()='Download']")
            wait_until(
                lambda: len(self.environment.downloads()) == count,
                20,
                f"download {count}",
            )
        names = self.environment.downloads()
        self.assertTrue(all(name.endswith(".zip") for name in names), names)
        self.assertEqual(len(set(names)), 2)

    def test_hands_other_addresses_to_the_system_and_refuses_the_rest(self):
        self.driver.go("/settings/stremio")
        self.driver.click("//a[starts-with(@href,'stremio:')]")
        for href in ("https://example.invalid/page", "file:///etc/hostname"):
            self.driver.script(
                "document.querySelector('#link')?.remove();"
                "const link = document.createElement('a');"
                "link.id = 'link'; link.href = arguments[0]; link.textContent = 'link';"
                "link.style = 'position:fixed;top:300px;left:300px;z-index:9999;"
                "font-size:40px';"
                "document.body.append(link);",
                href,
            )
            self.driver.click("//a[@id='link']")
            time.sleep(1)
            self.assertTrue(
                self.driver.script("return location.origin === arguments[0]", ORIGIN),
                href,
            )
        links = wait_until(
            lambda: len(self.environment.opened_links()) >= 2
            and self.environment.opened_links(),
            10,
            "the links to open",
        )
        self.assertEqual(links[0].split(":")[0], "stremio")
        self.assertEqual(links[1], "https://example.invalid/page")
        self.assertEqual(len(links), 2, "file: must not be opened")

    def drop(self, path):
        source = subprocess.Popen(
            [sys.executable, "-c", DRAG_SOURCE, f"file://{path}"],
            env=self.environment.variables,
            stdout=subprocess.PIPE,
            text=True,
        )
        try:
            self.assertEqual(source.stdout.readline().strip(), "ready")
            time.sleep(1)
            drag((1100, 675), (640, 400))
            time.sleep(1)
        finally:
            source.terminate()
            source.wait(10)
            source.stdout.close()

    def test_opens_a_dropped_torrent_file(self):
        path = write_torrent(self.environment.root, "Invented Dropped")
        self.driver.go("/library")
        time.sleep(1)
        self.drop(path)
        self.driver.wait_for_add_page("Invented Dropped")

    def test_ignores_any_other_dropped_file(self):
        path = os.path.join(self.environment.root, "invented.mkv")
        with open(path, "wb") as file:
            file.write(b"invented")
        self.driver.go("/library")
        time.sleep(1)
        self.drop(path)
        # As long as a torrent file is given to open, and the page must still be here.
        deadline = time.monotonic() + 20
        while time.monotonic() < deadline:
            self.assertEqual(self.driver.script("return location.pathname"), "/library")
            time.sleep(0.5)

    def test_says_why_another_dropped_file_cannot_be_added(self):
        path = os.path.join(self.environment.root, "invented.mkv")
        with open(path, "wb") as file:
            file.write(b"invented")
        self.driver.go("/add")
        time.sleep(1)
        self.drop(path)
        self.driver.wait_for_add_page("This torrent file could not be read.")

    def test_a_second_launch_opens_what_it_names(self):
        self.driver.go("/library")
        time.sleep(1)
        path = write_torrent(self.environment.root, "Invented Second")
        subprocess.run(
            [BINARY, *self.environment.arguments, path],
            env=self.environment.variables,
            timeout=20,
            check=True,
        )
        self.driver.wait_for_add_page("Invented Second")
        self.driver.go("/library")
        time.sleep(1)
        subprocess.run(
            [
                BINARY,
                *self.environment.arguments,
                "magnet:?xt=urn:btih:0123456789abcdef0123456789abcdef01234567&dn=Invented+Magnet",
            ],
            env=self.environment.variables,
            timeout=20,
            check=True,
        )
        self.driver.wait_for_add_page("Invented Magnet")

    def test_a_second_launch_leaves_the_first_running(self):
        second = subprocess.run(
            [BINARY, *self.environment.arguments],
            env=self.environment.variables,
            capture_output=True,
            timeout=20,
            check=False,
        )
        self.assertEqual(second.returncode, 0)
        self.assertEqual(self.driver.script("return 1"), 1)


class CommandLineTests(unittest.TestCase):
    """What needs no window."""

    def run_it(self, *arguments):
        return subprocess.run(
            [BINARY, *arguments], capture_output=True, text=True, timeout=20, check=False
        )

    def test_says_its_version_and_help(self):
        self.assertTrue(self.run_it("--version").stdout.startswith("torrplay-desktop "))
        shown = self.run_it("--help")
        self.assertEqual(shown.returncode, 0)
        self.assertTrue(shown.stdout.startswith("torrplay-desktop takes the options"))

    def test_refuses_other_commands(self):
        self.assertEqual(self.run_it("password", "remove").returncode, 2)
        self.assertEqual(self.run_it("--invented").returncode, 2)


class LifecycleTests(unittest.TestCase):
    """Starting and stopping, each with a fresh application and no WebDriver."""

    def setUp(self):
        self.environment = Environment()
        self.windows = Windows()
        self.running = []

    def tearDown(self):
        for process in self.running:
            if process.poll() is None:
                process.kill()
                process.wait(10)
        self.environment.kill_processes()
        wait_until(lambda: not listening(), 20, "the port to be free")
        self.environment.remove()

    def start(self, extra_arguments=()):
        process = self.environment.start(extra_arguments)
        self.running.append(process)
        return process

    def start_and_open(self):
        process = self.start()
        wait_until(listening, READY_SECONDS, "the server")
        wait_until(self.windows.closable, READY_SECONDS, "the window")
        wait_until(
            lambda: "the engine is ready, with the stored torrents"
            in self.environment.log_messages(),
            READY_SECONDS,
            "the engine",
        )
        return process

    def assert_stopped_cleanly(self):
        messages = self.environment.log_messages()
        self.assertIn("recording the clean shutdown", messages)
        self.assertEqual(messages[-1], "stopped")

    def test_closing_the_window_stops_cleanly(self):
        process = self.start_and_open()
        self.windows.close_all()
        self.assertEqual(process.wait(STOP_SECONDS), 0)
        self.assert_stopped_cleanly()

    def test_a_termination_signal_stops_cleanly_and_the_next_run_trusts_it(self):
        process = self.start_and_open()
        process.send_signal(signal.SIGTERM)
        self.assertEqual(process.wait(STOP_SECONDS), 0)
        self.assert_stopped_cleanly()
        next_run = self.start_and_open()
        self.assertNotIn(
            "the last run did not shut down cleanly; disk torrents check their data in full",
            self.environment.log_messages(),
        )
        next_run.send_signal(signal.SIGTERM)
        next_run.wait(STOP_SECONDS)

    def test_a_taken_port_shows_a_dialog_and_exits_with_an_error(self):
        with socket.socket() as holder:
            holder.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            holder.bind(("127.0.0.1", PORT))
            holder.listen(1)
            process = self.start()
            wait_until(self.windows.closable, READY_SECONDS, "the dialog")
            time.sleep(2)
            self.assertIsNone(process.poll(), "the dialog waits for an answer")

            # Again until it ends: a helper window can come up before the dialog does.
            def answered():
                self.windows.close_all()
                return process.poll() is not None

            wait_until(answered, 20, "the dialog to close")
            self.assertEqual(process.returncode, 1)

    def test_launching_while_stopping_starts_a_new_run_without_a_dialog(self):
        process = self.start_and_open()
        # A connection that does not finish, so stopping takes a while.
        with socket.create_connection(("127.0.0.1", PORT)) as stuck:
            stuck.send(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            time.sleep(0.3)
            self.windows.close_all()
            # Long enough to be stopping: a window's helpers stay listed, so this can't wait
            # for the list to empty.
            time.sleep(1)
            # Either the first takes this as a second launch and starts a new run when it
            # has stopped, or it has already stopped and this is the new run.
            self.start()
            # The first ends with no dialog to wait on.
            process.wait(STOP_SECONDS)
        wait_until(listening, READY_SECONDS, "a new run")
        wait_until(self.windows.closable, READY_SECONDS, "a new window")

    def test_a_taken_peer_port_shows_a_dialog_and_exits_with_an_error(self):
        with socket.socket() as holder:
            holder.setsockopt(socket.SOL_SOCKET, socket.SO_REUSEADDR, 1)
            holder.bind(("0.0.0.0", 27913))
            holder.listen(1)
            process = self.start()
            wait_until(
                lambda: "torrplay failed" in self.log_messages_if_any(),
                READY_SECONDS,
                "the failure",
            )
            self.assertIsNone(process.poll(), "the dialog waits for an answer")

            def answered():
                self.windows.close_all()
                return process.poll() is not None

            wait_until(answered, 20, "the dialog to close")
            self.assertEqual(process.returncode, 1)

    def log_messages_if_any(self):
        try:
            return self.environment.log_messages()
        except FileNotFoundError:
            return []

    def test_launching_during_a_termination_signal_starts_a_new_run(self):
        process = self.start_and_open()
        with socket.create_connection(("127.0.0.1", PORT)) as stuck:
            stuck.send(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            time.sleep(0.3)
            process.send_signal(signal.SIGTERM)
            time.sleep(0.5)
            self.start()
            process.wait(STOP_SECONDS)
        wait_until(listening, READY_SECONDS, "a new run")

    def test_a_second_launch_says_when_a_file_cannot_be_read(self):
        self.start_and_open()
        time.sleep(1)
        before = len(self.windows.closable())
        second = subprocess.run(
            [BINARY, *self.environment.arguments, "/invented/missing.torrent"],
            env=self.environment.variables,
            timeout=20,
            check=False,
        )
        self.assertEqual(second.returncode, 0)
        wait_until(lambda: len(self.windows.closable()) > before, 20, "the notice")

    def test_a_launch_while_stopping_opens_its_file_in_the_new_run(self):
        process = self.start_and_open()
        path = write_torrent(self.environment.root, "Invented Later")
        with socket.create_connection(("127.0.0.1", PORT)) as stuck:
            stuck.send(b"GET / HTTP/1.1\r\nHost: 127.0.0.1\r\n")
            time.sleep(0.3)
            self.windows.close_all()
            time.sleep(0.5)
            second = self.start([path])
            second.wait(20)
            process.wait(STOP_SECONDS)

        def new_run_names_the_file():
            listed = subprocess.run(
                ["pgrep", "-a", "-f", re.escape(self.environment.data)],
                capture_output=True,
                text=True,
                check=False,
            ).stdout
            return path in listed

        # A launch that came after the first had stopped is a new run itself, which has
        # no file to open; this one must have come while it was stopping.
        if second.returncode == 0 and not listening():
            wait_until(new_run_names_the_file, READY_SECONDS, "a new run with the file")
        wait_until(listening, READY_SECONDS, "a new run")

    def test_opens_what_its_command_line_names(self):
        path = write_torrent(self.environment.root, "Invented Launched")
        driver = Driver(self.environment, [path])
        try:
            driver.wait_for_add_page("Invented Launched")
        finally:
            driver.stop()

    def test_a_failed_download_is_reported(self):
        os.makedirs(os.path.join(self.environment.home, "Downloads"), mode=0o500)
        environment = self.environment
        driver = Driver(environment)
        try:
            driver.go("/settings/backup")
            before = len(self.windows.closable())
            driver.click("//button[normalize-space()='Download']")
            wait_until(
                lambda: len(self.windows.closable()) > before, 20, "the notice"
            )
            self.assertEqual(environment.downloads(), [])
        finally:
            driver.stop()
            os.chmod(os.path.join(environment.home, "Downloads"), stat.S_IRWXU)


def main():
    global BINARY
    if len(sys.argv) < 2:
        sys.exit(f"usage: {sys.argv[0]} PATH_TO_BINARY [unittest arguments]")
    BINARY = os.path.abspath(sys.argv[1])
    for port in (PORT, DRIVER_PORT):
        with socket.socket() as probe:
            if probe.connect_ex(("127.0.0.1", port)) == 0:
                sys.exit(f"port {port} is in use, perhaps by what an earlier run left")
    display = subprocess.Popen(
        ["Xvfb", DISPLAY, "-screen", "0", "1280x800x24"],
        stdout=subprocess.DEVNULL,
        stderr=subprocess.DEVNULL,
    )
    try:
        wait_until(
            lambda: os.path.exists(f"/tmp/.X11-unix/X{DISPLAY[1:]}"), 10, "Xvfb"
        )
        program = unittest.main(argv=[sys.argv[0], *sys.argv[2:]], exit=False)
    finally:
        display.terminate()
        display.wait(10)
    sys.exit(not program.result.wasSuccessful())


if __name__ == "__main__":
    main()
