#!/usr/bin/env python3
# SPDX-FileCopyrightText: 2026 TorrPlay
#
# SPDX-License-Identifier: MIT

"""Drives the service edition's client, built without the engine, on a virtual display.

Run it as `dbus-run-session -- python3 tests/client_smoke.py PATH_TO_CLIENT`. A small
HTTP server stands in for the service, so the test is of the client alone: that it
opens the service's page, says when the service does not answer, hands the page what
it is asked to open, and leaves the service running when it closes. It needs what
`smoke.py` needs.
"""

import http.server
import os
import subprocess
import threading
import time
import unittest

import smoke
from smoke import Windows, wait_until, write_torrent

# Keeps what it is handed as the client's page does, which reads it at once, so a
# reload does not lose it.
# The client's name to the system, which names its log directory.
IDENTIFIER = "org.torrplay.desktop"

PAGE = b"""<!doctype html><title>Invented service</title><p>Invented service</p>
<script>
function keep() {
  const names = JSON.parse(sessionStorage.getItem("opened") ?? "[]");
  for (const item of window.torrplayInbox?.splice(0) ?? []) names.push(item.name);
  sessionStorage.setItem("opened", JSON.stringify(names));
}
keep();
window.addEventListener("torrplay-inbox", keep);
</script>"""


class Service:
    """Something answering on the service's port, as the service would."""

    def __init__(self):
        self.pages = 0
        # What else the client asked of it, such as to stop, which it must not.
        self.other_requests = []
        service = self

        class Page(http.server.BaseHTTPRequestHandler):
            def refuse(self):
                service.other_requests.append(f"{self.command} {self.path}")
                self.send_response(405)
                self.send_header("Content-Length", "0")
                self.end_headers()

            do_POST = do_PUT = do_PATCH = do_DELETE = refuse

            def do_GET(self):
                service.pages += 1
                self.send_response(200)
                self.send_header("Content-Type", "text/html")
                self.send_header("Content-Length", str(len(PAGE)))
                self.end_headers()
                self.wfile.write(PAGE)

            def log_message(self, *_):
                pass

        self.server = http.server.ThreadingHTTPServer(("127.0.0.1", smoke.PORT), Page)
        self.thread = threading.Thread(target=self.server.serve_forever, daemon=True)
        self.thread.start()

    def stop(self):
        self.server.shutdown()
        self.server.server_close()
        self.thread.join(10)


class ClientEnvironment(smoke.Environment):
    """A private home, and the client under a name in it, which its processes alone
    carry on their command lines."""

    def __init__(self):
        super().__init__()
        self.binary = os.path.join(self.root, "bin", "torrplay-desktop")
        os.symlink(smoke.BINARY, self.binary)
        self.marker = self.binary
        # A settings file of its own, absent unless a test writes it, so a
        # service installed on this computer is not the one the client finds.
        self.settings = os.path.join(self.root, "torrplay.env")
        self.variables["TORRPLAY_SETTINGS_FILE"] = self.settings
        self.variables["TORRPLAY_LISTEN"] = f"127.0.0.1:{smoke.PORT}"
        self.arguments = []


class ClientDriver(smoke.Driver):
    """A WebDriver session on the client, which leaves the service alone on stopping."""

    def stop(self):
        self.process.send_signal(smoke.signal.SIGTERM)
        self.process.wait(10)
        self.environment.kill_processes()

    def inbox(self):
        return self.script("return JSON.parse(sessionStorage.getItem('opened') ?? '[]')")


class ClientWindowTests(unittest.TestCase):
    """The client's window on the service's page, in one session."""

    @classmethod
    def setUpClass(cls):
        cls.service = Service()
        cls.environment = ClientEnvironment()
        cls.torrent = write_torrent(cls.environment.root, "Invented Named")
        cls.driver = ClientDriver(cls.environment, [cls.torrent])

    @classmethod
    def tearDownClass(cls):
        cls.driver.stop()
        cls.service.stop()
        cls.environment.remove()

    def test_opens_the_services_page(self):
        wait_until(
            lambda: self.driver.script("return document.title") == "Invented service",
            30,
            "the service's page",
        )
        self.assertEqual(self.driver.script("return location.origin"), smoke.ORIGIN)

    def test_hands_the_page_what_its_command_line_names(self):
        wait_until(
            lambda: "Invented Named.torrent" in self.driver.inbox(),
            30,
            "the named file in the page's inbox",
        )

    def test_a_second_launch_hands_over_what_it_names(self):
        # Once the first file is in: before the page loads, only the newest waits.
        wait_until(
            lambda: "Invented Named.torrent" in self.driver.inbox(),
            30,
            "the named file in the page's inbox",
        )
        path = write_torrent(self.environment.root, "Invented Again")
        second = self.environment.start([path])
        self.assertEqual(second.wait(20), 0)
        wait_until(
            lambda: "Invented Again.torrent" in self.driver.inbox(),
            30,
            "the second launch's file in the page's inbox",
        )


class ClientLifecycleTests(unittest.TestCase):
    """Starting and ending the client, each time on its own, with no WebDriver."""

    def setUp(self):
        self.environment = ClientEnvironment()
        self.windows = Windows()

    def tearDown(self):
        self.environment.kill_processes()
        self.environment.remove()

    def close_until_ended(self, process):
        # Again until it ends: a helper window can come up before the dialog does.
        def ended():
            self.windows.close_all()
            return process.poll() is not None

        wait_until(ended, 20, "the client to end")
        return process.returncode

    def test_says_when_the_service_does_not_answer(self):
        process = self.environment.start()
        wait_until(self.windows.closable, smoke.READY_SECONDS, "the dialog")
        time.sleep(2)
        self.assertIsNone(process.poll(), "the dialog waits for an answer")
        self.assertEqual(self.close_until_ended(process), 1)

    def test_closing_the_window_ends_the_client_and_asks_nothing_of_the_service(self):
        service = Service()
        try:
            process = self.environment.start()
            # The page, not a window's helper, which comes long before it.
            wait_until(lambda: service.pages, smoke.READY_SECONDS, "the page")
            self.assertEqual(self.close_until_ended(process), 0)
            self.assertEqual(service.other_requests, [])
        finally:
            service.stop()

    def test_finds_the_service_at_the_settings_files_address_first(self):
        # The client's own variable names a port nothing listens on; the
        # settings file, which comes first, names the service's.
        self.environment.variables["TORRPLAY_LISTEN"] = f"127.0.0.1:{smoke.PORT + 1}"
        with open(self.environment.settings, "w", encoding="utf-8") as file:
            file.write(f"# Invented settings\nTORRPLAY_LISTEN=127.0.0.1:{smoke.PORT}\n")
        service = Service()
        try:
            process = self.environment.start()
            wait_until(lambda: service.pages, smoke.READY_SECONDS, "the page")
            self.assertEqual(self.close_until_ended(process), 0)
        finally:
            service.stop()

    def test_says_when_the_listen_address_cannot_be_read(self):
        self.environment.variables["TORRPLAY_LISTEN"] = "every interface"
        process = self.environment.start()
        wait_until(self.windows.closable, smoke.READY_SECONDS, "the dialog")
        time.sleep(2)
        self.assertIsNone(process.poll(), "the dialog waits for an answer")
        self.assertEqual(self.close_until_ended(process), 1)

    def test_logs_where_it_found_the_service(self):
        service = Service()
        try:
            process = self.environment.start()
            log = os.path.join(
                self.environment.variables["XDG_DATA_HOME"],
                IDENTIFIER,
                "logs",
                "client.log",
            )

            def logged():
                if not os.path.exists(log):
                    return False
                with open(log, encoding="utf-8") as file:
                    return "opening the service's page" in file.read()

            wait_until(logged, smoke.READY_SECONDS, "the client's log")
            self.close_until_ended(process)
        finally:
            service.stop()

    def test_says_its_version_and_help_and_refuses_the_services_options(self):
        def run_it(*arguments):
            return subprocess.run(
                [self.environment.binary, *arguments],
                capture_output=True,
                text=True,
                timeout=20,
                check=False,
            )

        self.assertTrue(run_it("--version").stdout.startswith("torrplay-desktop "))
        self.assertTrue(run_it("--help").stdout.startswith("Usage: torrplay-desktop"))
        self.assertEqual(run_it("--data-dir", "/invented").returncode, 2)


if __name__ == "__main__":
    smoke.main()
