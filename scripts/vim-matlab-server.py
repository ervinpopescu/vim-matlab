#!/usr/bin/env python

import os
import random
import signal
import socketserver
import string
import sys
import threading
import time
from sys import stdin

use_pexpect = True
if use_pexpect:
    try:
        import pexpect
    except ImportError:
        use_pexpect = False
if not use_pexpect:
    from subprocess import PIPE, Popen

hide_until_newline = False
auto_restart = True
server = None


class Matlab:
    def __init__(self):
        self.launch_process()

    def launch_process(self):
        shell_cmd = "xrdb -load ~/.config/X11/Xresources; env QT_QPA_PLATFORM=xcb LD_PRELOAD=/usr/lib/libstdc++.so:/usr/lib/libfreetype.so LD_LIBRARY_PATH=/usr/lib/dri/ matlab -nodesktop -nosplash -webui"
        if use_pexpect:
            self.proc = pexpect.spawn("/bin/bash", ["-c", shell_cmd])
            # self.proc = pexpect.spawn()
        else:
            self.proc = Popen(
                shell_cmd,
                stdin=PIPE,
                close_fds=True,
                preexec_fn=os.setsid,
            )
        return self.proc

    def cancel(self):
        os.kill(self.proc.pid, signal.SIGINT)  # ty:ignore[invalid-argument-type]

    def kill(self):
        try:
            os.killpg(self.proc.pid, signal.SIGTERM)  # ty:ignore[invalid-argument-type]
        except Exception as e:
            raise Exception("Could not kill process: {}".format(e))

    def run_code(self, code, run_timer=True):
        num_retry = 0
        rand_var = "".join(random.choice(string.ascii_uppercase) for _ in range(12))

        if run_timer:
            command = (
                "{randvar}=tic;{code},try,toc({randvar}),catch,end"
                ",clear('{randvar}');\n"
            ).format(randvar=rand_var, code=code.strip())
        else:
            command = "{}\n".format(code.strip())

        # The maximum number of characters allowed on a single line in Matlab's CLI is 4096.
        delim = " ...\n"
        line_size = 4095 - len(delim)
        command = delim.join(
            [command[i : i + line_size] for i in range(0, len(command), line_size)]
        )

        global hide_until_newline
        while num_retry < 3:
            try:
                if use_pexpect:
                    hide_until_newline = True
                    self.proc.send(command)  # ty:ignore[possibly-missing-attribute]
                else:
                    self.proc.stdin.write(command.encode())  # ty:ignore[possibly-missing-attribute]
                    self.proc.stdin.flush()  # ty:ignore[possibly-missing-attribute]
                break
            except Exception as ex:
                print(ex)
                self.launch_process()
                num_retry += 1
                time.sleep(1)


class TCPHandler(socketserver.StreamRequestHandler):
    def handle(self):
        print_flush("New connection: {}".format(self.client_address))

        while True:
            msg = self.rfile.readline()
            if not msg:
                break
            msg = msg.strip().decode("utf-8")
            print_flush((msg[:74] + "...") if len(msg) > 74 else msg, end="")

            options = {
                "kill": self.server.matlab.kill,  # ty:ignore[unresolved-attribute]
                "cancel": self.server.matlab.cancel,  # ty:ignore[unresolved-attribute]
            }

            if msg in options:
                options[msg]()
            else:
                self.server.matlab.run_code(msg)  # ty:ignore[unresolved-attribute]
        print_flush("Connection closed: {}".format(self.client_address))


def status_monitor_thread(matlab):
    while True:
        matlab.proc.wait()
        if not auto_restart:
            break
        print_flush("Restarting...")
        matlab.launch_process()
        start_thread(target=forward_input, args=(matlab,))
        time.sleep(1)

    global server
    server.shutdown()
    server.server_close()


def output_filter(output_string):
    """
    If the global variable `hide_until_newline` is True, this function will
    return an empty string until it sees a string that contains a newline.
    Used with `pexpect.spawn.interact` and `pexpect.spawn.send` to hide the
    raw command from being shown.

    :param output_string: String forwarded from MATLAB's stdout. Provided
    by `pexpect.spawn.interact`.
    :return: The filtered string.
    """
    global hide_until_newline
    newline = b"\n" if isinstance(output_string, bytes) else "\n"
    empty = b"" if isinstance(output_string, bytes) else ""
    if hide_until_newline:
        if newline in output_string:
            hide_until_newline = False
            return output_string[output_string.find(newline) :]
        else:
            return empty
    else:
        return output_string


def input_filter(input_string):
    # Detect C-\
    if input_string in ("\x1c", b"\x1c"):
        print_flush("Terminating")
        global auto_restart
        auto_restart = False
    return input_string


def forward_input(matlab):
    """Forward stdin to Matlab.proc's stdin."""
    if use_pexpect:
        matlab.proc.interact(input_filter=input_filter, output_filter=output_filter)
    else:
        while True:
            matlab.proc.stdin.write(stdin.readline().encode())


def start_thread(target=None, args=()):
    thread = threading.Thread(target=target, args=args)
    thread.daemon = True
    thread.start()


def print_flush(value, end="\n"):
    """Manually flush the line if using pexpect."""
    if isinstance(value, bytes):
        value = value.decode("utf-8", errors="replace")
    if use_pexpect:
        value += "\b" * len(value)
    sys.stdout.write(value + end)
    sys.stdout.flush()


def main():
    host, port = "localhost", 43889
    socketserver.TCPServer.allow_reuse_address = True

    global server
    server = socketserver.TCPServer((host, port), TCPHandler)
    server.matlab = Matlab()  # ty:ignore[unresolved-attribute]

    start_thread(target=forward_input, args=(server.matlab,))  # ty:ignore[unresolved-attribute]
    start_thread(target=status_monitor_thread, args=(server.matlab,))  # ty:ignore[unresolved-attribute]

    print_flush("Started server: {}".format((host, port)))
    server.serve_forever()


if __name__ == "__main__":
    main()
