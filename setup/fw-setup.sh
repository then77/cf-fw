#!/bin/sh
set -u
umask 077

stdout_color=0
stderr_color=0
if [ -z "${NO_COLOR+x}" ]; then
    [ -t 1 ] && stdout_color=1
    [ -t 2 ] && stderr_color=1
fi

style_stdout() {
    if [ "$stdout_color" -eq 1 ]; then
        printf '\033[%sm%s\033[0m' "$1" "$2"
    else
        printf '%s' "$2"
    fi
}

shell_error() {
    if [ "$stderr_color" -eq 1 ]; then
        printf '\033[41;1m ERROR \033[0m %s\n' "$1" >&2
    else
        printf ' ERROR  %s\n' "$1" >&2
    fi
}

shell_warning() {
    if [ "$stderr_color" -eq 1 ]; then
        printf '\033[43;30;1m WARNING \033[0m %s\n' "$1" >&2
    else
        printf ' WARNING  %s\n' "$1" >&2
    fi
}

portable_warning() {
    if [ "$stdout_color" -eq 1 ]; then
        printf '\033[43;30;1m WARN \033[0m Setup will store FW config in '
        style_stdout '1' "$1/"
    else
        printf ' WARN  Setup will store FW config in %s/' "$1"
    fi
    printf '%s\n' " instead of $2/. This is intended for portable setup mode and is not recommended for normal use. Continue if you know what you're doing."
}

shell_prompt() {
    style_stdout '34;1' "$1"
}

shell_status() {
    style_stdout '34;1' "$1"
    printf '\n'
}

welcome() {
    printf '%s' 'Welcome to '
    style_stdout '34;1' 'FW Automated Setup'
    printf '%s\n\n' '. This wizard will help you to:'
    style_stdout '2' ' •'
    printf '%s' ' Connect to your '
    style_stdout '34;1' 'Cloudflare account'
    printf '\n'
    style_stdout '2' ' •'
    printf '%s\n' ' Configure a wildcard domain'
    style_stdout '2' ' •'
    printf '%s' ' Create a '
    style_stdout '32;1' 'Cloudflare Tunnel'
    printf '\n'
    style_stdout '2' ' •'
    printf '%s' ' Download the official '
    style_stdout '33;1' 'cloudflared'
    printf '%s\n' ' binary'
    style_stdout '2' ' •'
    printf '%s\n\n' ' Verify that your tunnel is working'
    printf '%s\n' "You'll be asked to authorize with Cloudflare and configure your domain during setup."
    printf '%s\n\n' 'Additional configuration prompts may appear depending on your selections.'
}

shell_usage() {
    printf '%s\n' 'Usage: fw-setup.sh --fw-path <absolute-path> [--portable]' >&2
}

portable=0
fw_path=''
expect_fw_path=0
for bootstrap_arg do
    if [ "$expect_fw_path" -eq 1 ]; then
        case $bootstrap_arg in
            --*) shell_error '--fw-path requires a value.'; shell_usage; exit 2 ;;
        esac
        fw_path=$bootstrap_arg
        expect_fw_path=0
        continue
    fi
    case $bootstrap_arg in
        --fw-path) expect_fw_path=1 ;;
        --fw-path=*) fw_path=${bootstrap_arg#--fw-path=} ;;
        --portable) portable=1 ;;
        *) shell_error "Unknown argument: $bootstrap_arg"; shell_usage; exit 2 ;;
    esac
done
if [ "$expect_fw_path" -eq 1 ] || [ -z "$fw_path" ]; then
    shell_error '--fw-path is required.'
    shell_usage
    exit 2
fi
case $fw_path in
    /*) ;;
    *) shell_error '--fw-path must be an absolute path.'; shell_usage; exit 2 ;;
esac
if [ ! -f "$fw_path" ]; then
    shell_error "FW executable was not found: $fw_path"
    exit 1
fi
if [ ! -x "$fw_path" ]; then
    shell_error "FW path is not executable: $fw_path"
    exit 1
fi

welcome
install_manager=''
install_package=''
run_prefix=''

python_usable() {
    command -v python3 >/dev/null 2>&1 || return 1
    python3 -c 'import sys; raise SystemExit(0 if sys.version_info >= (3, 9) else 1)' >/dev/null 2>&1
}

if command -v python3 >/dev/null 2>&1; then
    if ! python_usable; then
        shell_error 'python3 is installed but is older than Python 3.9.'
        printf '%s\n' 'Upgrade it using your normal system administration process, then run setup again.' >&2
        printf '%s\n' 'FW setup did not replace or uninstall the existing Python installation.' >&2
        exit 1
    fi
else
    printf '%s\n' 'Python 3.9 or newer is required only while FW setup runs.'
    printf '%s\n' 'Because python3 is absent, setup can install it now with your permission.'
    printf '%s\n' 'The installed Python package will remain available after setup finishes.'
    shell_prompt 'Continue setup? (Y/n) '
    IFS= read -r bootstrap_answer || bootstrap_answer='n'
    case $bootstrap_answer in ''|y|Y|yes|YES|Yes) ;; *) printf '%s\n' 'Setup stopped.'; exit 1 ;; esac

    if command -v brew >/dev/null 2>&1; then
        install_manager='brew'; install_package='python'
    elif command -v apt-get >/dev/null 2>&1; then
        install_manager='apt-get'; install_package='python3'
    elif command -v dnf >/dev/null 2>&1; then
        install_manager='dnf'; install_package='python3'
    elif command -v yum >/dev/null 2>&1; then
        install_manager='yum'; install_package='python3'
    elif command -v pacman >/dev/null 2>&1; then
        install_manager='pacman'; install_package='python'
    elif command -v zypper >/dev/null 2>&1; then
        install_manager='zypper'; install_package='python3'
    elif command -v apk >/dev/null 2>&1; then
        install_manager='apk'; install_package='python3'
    else
        shell_error 'No supported Python package manager was found.'
        printf '%s\n' 'Install Python 3.9+ manually and run setup again.' >&2
        exit 1
    fi

    package_already_installed=1
    case $install_manager in
        brew) brew list --versions "$install_package" >/dev/null 2>&1 || package_already_installed=0 ;;
        apt-get)
            package_status=$(dpkg-query -W -f='${db:Status-Abbrev}' "$install_package" 2>/dev/null || true)
            [ "$package_status" = 'ii ' ] || package_already_installed=0 ;;
        dnf|yum|zypper) rpm -q "$install_package" >/dev/null 2>&1 || package_already_installed=0 ;;
        pacman) pacman -Q "$install_package" >/dev/null 2>&1 || package_already_installed=0 ;;
        apk) apk info -e "$install_package" >/dev/null 2>&1 || package_already_installed=0 ;;
    esac
    if [ "$package_already_installed" -eq 1 ]; then
        shell_error "$install_manager reports that $install_package is already installed, but python3 is unavailable."
        printf '%s\n' 'Fix PATH or the existing package installation, then run setup again.' >&2
        printf '%s\n' 'FW setup did not change or remove that pre-existing package.' >&2
        exit 1
    fi

    if [ "$install_manager" != 'brew' ] && [ "$(id -u)" != '0' ]; then
        if command -v sudo >/dev/null 2>&1; then
            run_prefix='sudo'
        else
            shell_error "$install_manager requires root privileges, and sudo is unavailable."
            printf '%s\n' 'Install Python 3.9+ manually and run setup again.' >&2
            exit 1
        fi
    fi

    shell_status "Installing setup-only dependency $install_package with $install_manager..."
    install_status=0
    case $install_manager in
        brew) brew install "$install_package" || install_status=$? ;;
        apt-get)
            if [ -n "$run_prefix" ]; then sudo apt-get update && sudo apt-get install -y "$install_package" || install_status=$?
            else apt-get update && apt-get install -y "$install_package" || install_status=$?; fi ;;
        dnf)
            if [ -n "$run_prefix" ]; then sudo dnf install -y "$install_package" || install_status=$?
            else dnf install -y "$install_package" || install_status=$?; fi ;;
        yum)
            if [ -n "$run_prefix" ]; then sudo yum install -y "$install_package" || install_status=$?
            else yum install -y "$install_package" || install_status=$?; fi ;;
        pacman)
            if [ -n "$run_prefix" ]; then sudo pacman -S --needed --noconfirm "$install_package" || install_status=$?
            else pacman -S --needed --noconfirm "$install_package" || install_status=$?; fi ;;
        zypper)
            if [ -n "$run_prefix" ]; then sudo zypper --non-interactive install "$install_package" || install_status=$?
            else zypper --non-interactive install "$install_package" || install_status=$?; fi ;;
        apk)
            if [ -n "$run_prefix" ]; then sudo apk add "$install_package" || install_status=$?
            else apk add "$install_package" || install_status=$?; fi ;;
    esac
    if [ "$install_status" -ne 0 ]; then
        shell_error "$install_manager could not install $install_package (exit $install_status)."
        exit "$install_status"
    fi
    if ! python_usable; then
        shell_error 'The installed python3 is missing or older than Python 3.9.'
        printf 'The %s package %s was left installed for inspection or future use.\n' "$install_manager" "$install_package" >&2
        exit 1
    fi
    shell_warning "Python package $install_package installed by $install_manager will remain installed after FW setup."
fi

if [ "$portable" -eq 1 ]; then
    fw_directory=${fw_path%/*}
    [ -n "$fw_directory" ] || fw_directory='/'
    portable_cf="${fw_directory%/}/cf"
    case $(uname -s) in
        Darwin) user_cf="${HOME:?HOME is required}/Library/Application Support/FW/cf" ;;
        *)
            case ${XDG_DATA_HOME:-} in
                /*) user_cf="${XDG_DATA_HOME}/fw/cf" ;;
                *) user_cf="${HOME:?HOME is required}/.local/share/fw/cf" ;;
            esac
            ;;
    esac
    portable_warning "$portable_cf" "$user_cf"
    printf '\n'
fi

shell_prompt 'To start the setup process, press enter.'
printf '\n'
IFS= read -r bootstrap_start || { shell_error 'Input ended before setup started.'; exit 1; }

python3 - "$@" 3<&0 <<'FW_SETUP_PYTHON'
from __future__ import annotations

import argparse
import atexit
import base64
import contextlib
import hashlib
import hmac
import html
import http.client
import http.server
import json
import os
import platform
import re
import secrets
import shutil
import signal
import socket
import ssl
import subprocess
import sys
import tarfile
import tempfile
import threading
import time
import urllib.error
import urllib.parse
import urllib.request
import webbrowser
import unicodedata
from pathlib import Path
from typing import Any, BinaryIO, Dict, Iterable, List, Optional, Tuple

# File descriptor 3 is the caller's original stdin; stdin itself carries this
# embedded source code to the interpreter.
try:
    sys.stdin = os.fdopen(3, "r", encoding="utf-8", errors="replace", closefd=False)
except OSError:
    pass

API_BASE = "https://api.cloudflare.com/client/v4"
AUTHORIZE_ENDPOINT = "https://dash.cloudflare.com/oauth2/auth"
TOKEN_ENDPOINT = "https://dash.cloudflare.com/oauth2/token"
REVOKE_ENDPOINT = "https://dash.cloudflare.com/oauth2/revoke"
OAUTH_CLIENT_ID = "c5ddfa8b3cab280893b3bcc428dcc7c9"
BASE_SCOPES = ["account-settings.read", "zone.read", "dns.write", "argotunnel.write", "ssl-and-certificates.read"]
WORKERS_SCOPE = "workers-scripts.write"
CLOUDFLARED_VERSION = "2026.9.1"
WORKER_COMPATIBILITY_DATE = "2026-09-17"
CERTIFICATE_POLL_SECONDS = 15
CERTIFICATE_POLL_ATTEMPTS = 40
WORKER_CERTIFICATE_ATTEMPTS = 4
CALLBACK_PORTS = (8976, 8977, 8978)
FAILURE_STATES = {"validation_timed_out", "issuance_timed_out", "deployment_timed_out", "deletion_timed_out", "initializing_timed_out"}
MAX_RESPONSE = 16 * 1024 * 1024
MAX_DOWNLOAD = 128 * 1024 * 1024
HOST_RE = re.compile(r"^\*\.(?=.{1,253}$)(?:[a-z0-9](?:[a-z0-9-]{0,61}[a-z0-9])?\.)+[a-z](?:[a-z0-9-]{0,61}[a-z0-9])?$")

# Asset digests are from GitHub release asset metadata. Darwin binary digests
# are the upstream release-note digests for the single extracted member.
ARTIFACTS = {
    ("darwin", "amd64"): ("cloudflared-darwin-amd64.tgz", "ff0d3b51d5ff70eceef89d6b32145fee985018a2174596a5dbe405e2766e2ac4", "1ea07ae775b03236bd6be18ca1848d6bdc4af2f4f3bce398823b5a36e5761b75", True),
    ("darwin", "arm64"): ("cloudflared-darwin-arm64.tgz", "c27ab8fd0aa489449e3d201eb02f957ef460a13b613662928b1b23394bf1bcfe", "9a0b19f67dc7a3011bc6b972c7ce06a5fcea8784ac6bd599ffa382ea4aeb5a6e", True),
    ("linux", "amd64"): ("cloudflared-linux-amd64", "03f1f25d1cc93b9ad6c60569d44060bc4f17ed97075760ed8cfca4b12dcd68cc", "03f1f25d1cc93b9ad6c60569d44060bc4f17ed97075760ed8cfca4b12dcd68cc", False),
    ("linux", "arm64"): ("cloudflared-linux-arm64", "3d97437c71848bd8df68041e12436b484a661d95073ea1937f01a845ce88faa3", "3d97437c71848bd8df68041e12436b484a661d95073ea1937f01a845ce88faa3", False),
    ("linux", "386"): ("cloudflared-linux-386", "5d66134cf7646cb98f33aeee7bcc8b97d8feacd76db279f5903f9585226e0922", "5d66134cf7646cb98f33aeee7bcc8b97d8feacd76db279f5903f9585226e0922", False),
    ("linux", "armhf"): ("cloudflared-linux-armhf", "95420507a720fb543122a5d69372fbde8f5c919790e95ddd4374a449e0a6f4dd", "95420507a720fb543122a5d69372fbde8f5c919790e95ddd4374a449e0a6f4dd", False),
}

class SetupError(Exception):
    pass

class Console:
    RESET = "\033[0m"
    BLUE_BOLD = "\033[34;1m"
    GREEN_BOLD = "\033[32;1m"
    YELLOW_BOLD = "\033[33;1m"
    DIM = "\033[2m"
    ERROR_LABEL = "\033[41;1m"
    WARNING_LABEL = "\033[43;30;1m"

    def __init__(self) -> None:
        self.status_active = False

    @staticmethod
    def enabled(stream: Any) -> bool:
        return "NO_COLOR" not in os.environ and bool(getattr(stream, "isatty", lambda: False)())

    @staticmethod
    def clean(value: Any) -> str:
        text = str(value)
        return "".join(character if not unicodedata.category(character).startswith("C") else " " for character in text)

    def styled(self, text: str, style: str, stream: Any = sys.stdout, sanitize: bool = False) -> str:
        value = self.clean(text) if sanitize else text
        if self.enabled(stream):
            return f"{style}{value}{self.RESET}"
        return value

    def line(self, text: Any = "", *, style: Optional[str] = None, stream: Any = sys.stdout, sanitize: bool = False, end: str = "\n", flush: bool = False) -> None:
        value = self.clean(text) if sanitize else str(text)
        if style:
            value = self.styled(value, style, stream)
        print(value, file=stream, end=end, flush=flush)

    def error(self, message: Any, stream: Any = sys.stderr) -> None:
        label = self.styled(" ERROR ", self.ERROR_LABEL, stream)
        self.line(f"{label} {self.clean(message)}", stream=stream)

    def warning(self, message: Any) -> None:
        label = self.styled(" WARNING ", self.WARNING_LABEL, sys.stderr)
        self.line(f"{label} {self.clean(message)}", stream=sys.stderr)

    def prompt(self, message: str) -> str:
        return input(self.styled(message, self.BLUE_BOLD))

    def heading(self, message: str) -> None:
        self.line(message, style=self.BLUE_BOLD)

    def status(self, message: Any) -> None:
        if self.status_active:
            self.fail()
        self.line(message, style=self.BLUE_BOLD, sanitize=True, end="", flush=True)
        self.status_active = True

    def ok(self, note: Any = "SUCCESS") -> None:
        self.line(f" {self.clean(note)}", style=self.GREEN_BOLD)
        self.status_active = False

    def fail(self) -> None:
        if self.status_active:
            self.line(" FAILED", style=self.ERROR_LABEL)
            self.status_active = False

    def success(self, message: str) -> None:
        self.line(message, style=self.GREEN_BOLD)

    def url(self, value: Any) -> None:
        self.line(value, style=self.DIM, sanitize=True)

    def detail(self, label: str, value: Any, end: str = "\n") -> None:
        self.line(f"{label}{self.clean(value)}", end=end)

CONSOLE = Console()

class BoundedRedirect(urllib.request.HTTPRedirectHandler):
    def __init__(self, limit: int = 5) -> None:
        self.limit = limit

    def redirect_request(self, req: urllib.request.Request, fp: BinaryIO, code: int, msg: str, headers: Any, newurl: str) -> Optional[urllib.request.Request]:
        count = int(getattr(req, "_fw_redirect_count", 0)) + 1
        if count > self.limit:
            raise SetupError("Too many HTTP redirects.")
        source = urllib.parse.urlsplit(req.full_url)
        target = urllib.parse.urlsplit(newurl)
        source_scheme = source.scheme.lower()
        target_scheme = target.scheme.lower()
        if source_scheme == "https" and target_scheme != "https":
            raise SetupError("Refusing an HTTPS redirect to a non-HTTPS URL.")
        if req.get_header("Authorization"):
            source_origin = (source_scheme, source.hostname, source.port or (443 if source_scheme == "https" else 80))
            target_origin = (target_scheme, target.hostname, target.port or (443 if target_scheme == "https" else 80))
            if source_origin != target_origin:
                raise SetupError("Refusing to forward authorization credentials to a different origin.")
        redirected = super().redirect_request(req, fp, code, msg, headers, newurl)
        if redirected is not None:
            setattr(redirected, "_fw_redirect_count", count)
        return redirected

OPENER = urllib.request.build_opener(BoundedRedirect())

def user_cf_directory() -> Path:
    if platform.system() == "Darwin":
        return Path.home() / "Library" / "Application Support" / "FW" / "cf"
    xdg_data_home = os.environ.get("XDG_DATA_HOME")
    if xdg_data_home and Path(xdg_data_home).is_absolute():
        return Path(xdg_data_home) / "fw" / "cf"
    return Path.home() / ".local" / "share" / "fw" / "cf"


def select_cf_directory(fw_path: Path, portable: bool) -> Path:
    adjacent = fw_path.parent / "cf"
    if portable or adjacent.exists() or adjacent.is_symlink():
        return adjacent
    return user_cf_directory()


class Setup:
    def __init__(self, fw_path: Path, portable: bool) -> None:
        self.fw_path = fw_path
        self.portable = portable
        self.temp = Path(tempfile.mkdtemp(prefix="fw-setup-"))
        os.chmod(self.temp, 0o700)
        self.tokens: List[str] = []
        self.access_token: Optional[str] = None
        self.account_id: Optional[str] = None
        self.zone_id: Optional[str] = None
        self.tunnel_id: Optional[str] = None
        self.dns_record_id: Optional[str] = None
        self.worker_name: Optional[str] = None
        self.worker_domain_id: Optional[str] = None
        self.worker_created = False
        self.created_files: List[Path] = []
        self.staged_files: List[Path] = []
        self.committed = False
        self.test_server: Optional[http.server.HTTPServer] = None
        self.test_thread: Optional[threading.Thread] = None
        self.fw_process: Optional[subprocess.Popen[bytes]] = None
        self.cf_dir: Optional[Path] = None
        self.cloudflared_path: Optional[Path] = None
        self.config_path: Optional[Path] = None
        self._cleaned = False
        atexit.register(self.cleanup_local)

    def cleanup_local(self) -> None:
        if self._cleaned:
            return
        self._cleaned = True
        self.stop_processes()
        shutil.rmtree(self.temp, ignore_errors=True)

    @staticmethod
    def status(message: str) -> None:
        CONSOLE.status(message)

    @staticmethod
    def ok(note: str = "SUCCESS") -> None:
        CONSOLE.ok(note)

    @staticmethod
    def json_bytes(value: Any) -> bytes:
        return json.dumps(value, separators=(",", ":"), ensure_ascii=False).encode("utf-8")

    @staticmethod
    def error_message(payload: Any, fallback: str) -> str:
        if isinstance(payload, dict):
            errors = payload.get("errors") or []
            details = "; ".join(f"[{item.get('code')}] {item.get('message')}" for item in errors if isinstance(item, dict))
            if details:
                return details
            return str(payload.get("error_description") or payload.get("error") or fallback)
        return fallback

    def request(self, url: str, method: str = "GET", data: Optional[bytes] = None, headers: Optional[Dict[str, str]] = None, timeout: int = 45, max_bytes: int = MAX_RESPONSE) -> Tuple[int, bytes]:
        request_headers = {"User-Agent": f"FW-Setup/{CLOUDFLARED_VERSION}"}
        if headers:
            request_headers.update(headers)
        request = urllib.request.Request(url, data=data, headers=request_headers, method=method)
        try:
            with OPENER.open(request, timeout=timeout) as response:
                body = response.read(max_bytes + 1)
                if len(body) > max_bytes:
                    raise SetupError(f"Response from {urllib.parse.urlsplit(url).hostname} exceeded the safety limit.")
                return response.status, body
        except urllib.error.HTTPError as error:
            body = error.read(max_bytes + 1)
            if len(body) > max_bytes:
                raise SetupError("HTTP error response exceeded the safety limit.") from None
            try:
                payload = json.loads(body)
            except Exception:
                payload = None
            raise SetupError(f"HTTP {error.code}: {self.error_message(payload, error.reason or 'request failed')}") from None
        except (urllib.error.URLError, TimeoutError, socket.timeout, ssl.SSLError) as error:
            raise SetupError(f"Network request failed: {error}") from None

    def request_json(self, url: str, method: str = "GET", body: Any = None, token: Optional[str] = None, timeout: int = 45) -> Any:
        headers = {"Accept": "application/json"}
        data = None
        if body is not None:
            data = self.json_bytes(body)
            headers["Content-Type"] = "application/json"
        if token:
            headers["Authorization"] = f"Bearer {token}"
        _, raw = self.request(url, method, data, headers, timeout)
        try:
            return json.loads(raw)
        except (UnicodeDecodeError, json.JSONDecodeError):
            raise SetupError("The server returned invalid JSON.") from None

    def cf_api(self, method: str, path: str, body: Any = None, envelope: bool = False) -> Any:
        if not self.access_token:
            raise SetupError("Cloudflare access token is unavailable.")
        payload = self.request_json(API_BASE + path, method, body, self.access_token)
        if not isinstance(payload, dict) or payload.get("success") is not True:
            raise SetupError(self.error_message(payload, "Cloudflare returned an unsuccessful API response."))
        return payload if envelope else payload.get("result")

    def all_pages(self, path: str) -> List[Any]:
        items: List[Any] = []
        page = 1
        while True:
            separator = "&" if "?" in path else "?"
            envelope = self.cf_api("GET", f"{path}{separator}page={page}&per_page=50", envelope=True)
            result = envelope.get("result") or []
            if not isinstance(result, list):
                raise SetupError("Cloudflare returned invalid pagination data.")
            items.extend(result)
            info = envelope.get("result_info") or {}
            if info.get("total_pages"):
                total_pages = int(info["total_pages"])
            elif info.get("total_count") is not None:
                per_page = int(info.get("per_page") or 50)
                total_pages = (int(info["total_count"]) + per_page - 1) // per_page
            else:
                total_pages = page + 1 if len(result) == 50 else page
            if page >= total_pages:
                return items
            page += 1

    def oauth_login(self, scopes: Iterable[str]) -> None:
        scope_list = list(dict.fromkeys(scopes))
        state = base64.urlsafe_b64encode(secrets.token_bytes(24)).rstrip(b"=").decode("ascii")
        verifier = base64.urlsafe_b64encode(secrets.token_bytes(64)).rstrip(b"=").decode("ascii")
        challenge = base64.urlsafe_b64encode(hashlib.sha256(verifier.encode("ascii")).digest()).rstrip(b"=").decode("ascii")
        result: Dict[str, str] = {}
        expected_state = state

        class CallbackServer(http.server.HTTPServer):
            allow_reuse_address = False

            def get_request(inner_self) -> Tuple[socket.socket, Any]:
                connection, address = super().get_request()
                connection.settimeout(10)
                return connection, address

        class Handler(http.server.BaseHTTPRequestHandler):
            protocol_version = "HTTP/1.1"
            def log_message(self, _format: str, *args: Any) -> None:
                return
            def respond(self, status: int, completed: bool, message: str) -> None:
                if completed:
                    title = "OAuth completed"
                    heading = "OAuth completed"
                    background = "#0f111a"
                else:
                    title = "OAuth Failed"
                    heading = "OAuth failed"
                    background = "#1a120f"
                body = ("<!doctype html><html lang=en><head><meta charset=utf-8>"
                        "<meta name=viewport content='width=device-width,initial-scale=1'>"
                        f"<title>{html.escape(title)}</title>"
                        "<style>:root{color-scheme:light dark;font-family:system-ui,sans-serif}"
                        f"body{{min-height:100vh;margin:0;display:grid;place-items:center;background:{background};color:#f9fafb}}"
                        "main{max-width:32rem;padding:2rem;text-align:center}"
                        "h1{font-size:clamp(2rem, 6vw, 3rem);margin:0 0 1rem}"
                        "p{color:#9ca3af;line-height:1.6}</style></head><body><main>"
                        f"<h1>{html.escape(heading)}</h1><p>{html.escape(message)}</p>"
                        "</main></body></html>").encode("utf-8")
                self.send_response(status)
                self.send_header("Content-Type", "text/html; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.send_header("Connection", "close")
                self.end_headers()
                self.wfile.write(body)
            def do_GET(self) -> None:
                parsed = urllib.parse.urlsplit(self.path)
                if parsed.path != "/oauth/callback":
                    result["error"] = "Unexpected OAuth callback path."
                    self.respond(404, False, "Not found.")
                    return
                values = urllib.parse.parse_qs(parsed.query, keep_blank_values=True)
                received_state = values.get("state", [""])[0]
                if not hmac.compare_digest(received_state, expected_state):
                    result["error"] = "OAuth state validation failed."
                    self.respond(400, False, "FW setup rejected this OAuth response.")
                    return
                if values.get("error"):
                    result["error"] = values.get("error_description", values["error"])[0]
                    self.respond(400, False, result["error"])
                    return
                code = values.get("code", [""])[0]
                if not code:
                    result["error"] = "Cloudflare returned no authorization code."
                    self.respond(400, False, result["error"])
                    return
                result["code"] = code
                self.respond(200, True, "You can close this window and return to FW Setup.")

        server: Optional[CallbackServer] = None
        for port in CALLBACK_PORTS:
            try:
                server = CallbackServer(("127.0.0.1", port), Handler)
                break
            except OSError:
                pass
        if server is None:
            raise SetupError("FW could not listen on OAuth callback ports 8976, 8977, or 8978.")
        server.timeout = 300
        redirect_uri = f"http://127.0.0.1:{server.server_port}/oauth/callback"
        query = urllib.parse.urlencode({"response_type": "code", "client_id": OAUTH_CLIENT_ID, "redirect_uri": redirect_uri, "scope": " ".join(scope_list), "state": state, "code_challenge": challenge, "code_challenge_method": "S256"})
        auth_uri = f"{AUTHORIZE_ENDPOINT}?{query}"
        CONSOLE.heading("Open this URL in your browser to authorize to Cloudflare:")
        CONSOLE.url(auth_uri)
        CONSOLE.line()
        try:
            webbrowser.open(auth_uri, new=1, autoraise=True)
            server.handle_request()
        finally:
            server.server_close()
        if "error" in result:
            raise SetupError(result["error"])
        if "code" not in result:
            raise SetupError("Timed out waiting for Cloudflare authorization.")
        form = urllib.parse.urlencode({"grant_type": "authorization_code", "client_id": OAUTH_CLIENT_ID, "code": result["code"], "redirect_uri": redirect_uri, "code_verifier": verifier}).encode("ascii")
        _, raw = self.request(TOKEN_ENDPOINT, "POST", form, {"Content-Type": "application/x-www-form-urlencoded"})
        try:
            token = json.loads(raw)
        except json.JSONDecodeError:
            raise SetupError("Cloudflare returned an invalid OAuth token response.") from None
        access_token = token.get("access_token") if isinstance(token, dict) else None
        if not access_token:
            raise SetupError(self.error_message(token, "Cloudflare returned no access token."))
        self.access_token = str(access_token)
        self.tokens.append(self.access_token)
        granted = str(token.get("scope") or "").split()
        if granted:
            missing = [scope for scope in scope_list if scope not in granted]
            if missing:
                raise SetupError(f"Cloudflare authorization did not grant every required permission: {', '.join(missing)}.")
        code = verifier = state = access_token = ""

    def revoke_tokens(self) -> None:
        for token in self.tokens:
            form = urllib.parse.urlencode({"token": token, "client_id": OAUTH_CLIENT_ID}).encode("ascii")
            try:
                self.request(REVOKE_ENDPOINT, "POST", form, {"Content-Type": "application/x-www-form-urlencoded"}, timeout=20)
            except Exception:
                pass
        self.tokens.clear()
        self.access_token = None

    @staticmethod
    def validate_release_configuration() -> None:
        values = [OAUTH_CLIENT_ID, *BASE_SCOPES, WORKERS_SCOPE]
        if any(not value.strip() or value.upper().startswith("REPLACE_") for value in values):
            raise SetupError("This setup release is not configured: its Cloudflare OAuth client ID or scope values have not been replaced.")

    def validate_paths(self) -> None:
        if not self.fw_path.is_absolute():
            raise SetupError("--fw-path must be an absolute path.")
        self.fw_path = self.fw_path.resolve(strict=True)
        if not self.fw_path.is_file():
            raise SetupError(f"FW executable was not found: {self.fw_path}")
        if not os.access(self.fw_path, os.X_OK):
            raise SetupError(f"FW path is not executable: {self.fw_path}")
        self.cf_dir = select_cf_directory(self.fw_path, self.portable)
        if self.cf_dir.is_symlink():
            raise SetupError(f"FW will not use a symbolic-link configuration directory: {self.cf_dir}")
        if self.cf_dir.exists() and not self.cf_dir.is_dir():
            raise SetupError(f"FW configuration path is not a directory: {self.cf_dir}")
        self.cf_dir.mkdir(mode=0o700, parents=True, exist_ok=True)
        os.chmod(self.cf_dir, 0o700)
        probe = self.cf_dir / f".write-{secrets.token_hex(8)}"
        try:
            fd = os.open(probe, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(fd)
        except OSError as error:
            raise SetupError(f"FW cannot write to {self.cf_dir}: {error}") from None
        finally:
            with contextlib.suppress(OSError):
                probe.unlink()
        self.config_path = self.cf_dir / "config.yml"
        if self.config_path.exists() or self.config_path.is_symlink():
            raise SetupError(f"FW will not overwrite the existing configuration: {self.config_path}")
        self.cloudflared_path = self.cf_dir / "cloudflared"

    @staticmethod
    def artifact() -> Tuple[str, str, str, bool]:
        system = platform.system().lower()
        machine = platform.machine().lower()
        architectures = {"x86_64": "amd64", "amd64": "amd64", "aarch64": "arm64", "arm64": "arm64", "i386": "386", "i486": "386", "i586": "386", "i686": "386", "x86": "386", "armv7l": "armhf", "armv7": "armhf", "armhf": "armhf"}
        key = (system, architectures.get(machine, machine))
        artifact = ARTIFACTS.get(key)
        if not artifact or not artifact[1] or not artifact[2]:
            raise SetupError(f"No pinned cloudflared checksum is available for {system}/{machine}; refusing an unverified download.")
        return artifact

    @staticmethod
    def hash_file(path: Path) -> str:
        digest = hashlib.sha256()
        with path.open("rb") as handle:
            for block in iter(lambda: handle.read(1024 * 1024), b""):
                digest.update(block)
        return digest.hexdigest()

    def download_to(self, url: str, destination: Path) -> None:
        request = urllib.request.Request(url, headers={"User-Agent": f"FW-Setup/{CLOUDFLARED_VERSION}"})
        try:
            with OPENER.open(request, timeout=300) as response, destination.open("wb") as output:
                content_length = response.headers.get("Content-Length")
                if content_length and int(content_length) > MAX_DOWNLOAD:
                    raise SetupError("cloudflared download exceeds the safety limit.")
                total = 0
                while True:
                    block = response.read(1024 * 1024)
                    if not block:
                        break
                    total += len(block)
                    if total > MAX_DOWNLOAD:
                        raise SetupError("cloudflared download exceeds the safety limit.")
                    output.write(block)
        except (urllib.error.URLError, TimeoutError, socket.timeout, OSError) as error:
            raise SetupError(f"Failed to download cloudflared: {error}") from None

    @staticmethod
    def extract_exact_tar(archive: Path, output: Path) -> None:
        try:
            with tarfile.open(archive, "r:gz") as bundle:
                members = bundle.getmembers()
                if len(members) != 1:
                    raise SetupError("The cloudflared archive contains unexpected members; refusing extraction.")
                member = members[0]
                if member.name != "cloudflared" or not member.isfile() or member.issym() or member.islnk() or member.size > MAX_DOWNLOAD:
                    raise SetupError("The cloudflared archive contains an unsafe member; refusing extraction.")
                source = bundle.extractfile(member)
                if source is None:
                    raise SetupError("Could not read cloudflared from its archive.")
                with source, output.open("wb") as target:
                    shutil.copyfileobj(source, target, 1024 * 1024)
        except (tarfile.TarError, OSError) as error:
            raise SetupError(f"Could not safely extract cloudflared: {error}") from None

    def install_cloudflared(self) -> None:
        assert self.cf_dir and self.cloudflared_path
        asset, asset_hash, binary_hash, archive = self.artifact()
        if self.cloudflared_path.exists() or self.cloudflared_path.is_symlink():
            if self.cloudflared_path.is_symlink() or not self.cloudflared_path.is_file():
                raise SetupError(f"Existing cloudflared path is not a regular file: {self.cloudflared_path}")
            if self.hash_file(self.cloudflared_path) != binary_hash:
                raise SetupError(f"An existing cloudflared binary has an unexpected SHA-256 hash. FW will not overwrite it: {self.cloudflared_path}")
            os.chmod(self.cloudflared_path, 0o755)
            return
        url = f"https://github.com/cloudflare/cloudflared/releases/download/{CLOUDFLARED_VERSION}/{asset}"
        CONSOLE.line("Cloudflared URL:", style=CONSOLE.YELLOW_BOLD)
        CONSOLE.url(url)
        self.status("Downloading cloudflared...")
        download = self.cf_dir / f".cloudflared-download-{secrets.token_hex(8)}"
        staged = self.cf_dir / f".cloudflared-install-{secrets.token_hex(8)}"
        self.staged_files.extend((download, staged))
        for path in (download, staged):
            fd = os.open(path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
            os.close(fd)
        self.download_to(url, download)
        actual = self.hash_file(download)
        if not hmac.compare_digest(actual, asset_hash):
            raise SetupError(f"cloudflared integrity check failed. Expected {asset_hash}, received {actual}.")
        if archive:
            self.extract_exact_tar(download, staged)
        else:
            shutil.copyfile(download, staged)
        binary_actual = self.hash_file(staged)
        if not hmac.compare_digest(binary_actual, binary_hash):
            raise SetupError(f"Extracted cloudflared integrity check failed. Expected {binary_hash}, received {binary_actual}.")
        os.chmod(staged, 0o755)
        os.replace(staged, self.cloudflared_path)
        self.staged_files.remove(staged)
        self.created_files.append(self.cloudflared_path)
        download.unlink()
        self.staged_files.remove(download)
        self.ok()

    def write_atomic(self, destination: Path, content: bytes, mode: int) -> None:
        if destination.exists() or destination.is_symlink():
            raise SetupError(f"FW will not overwrite the existing file: {destination}")
        staged = destination.parent / f".fw-file-{secrets.token_hex(8)}"
        self.staged_files.append(staged)
        fd = os.open(staged, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600)
        try:
            with os.fdopen(fd, "wb") as handle:
                handle.write(content)
                handle.flush()
                os.fsync(handle.fileno())
            os.chmod(staged, mode)
            os.replace(staged, destination)
            self.staged_files.remove(staged)
            self.created_files.append(destination)
        except Exception:
            with contextlib.suppress(OSError):
                os.close(fd)
            raise

    @staticmethod
    def choose(items: List[Any], label: str) -> Any:
        if not items:
            raise SetupError("No Cloudflare accounts are available to this authorization.")
        if len(items) == 1:
            return items[0]
        CONSOLE.line()
        CONSOLE.heading("Select a Cloudflare account:")
        for index, item in enumerate(items, 1):
            CONSOLE.detail(f"  [{index}] ", item.get(label, ""))
        while True:
            try:
                value = int(CONSOLE.prompt("Account number: "))
            except (ValueError, EOFError):
                value = 0
            if 1 <= value <= len(items):
                return items[value - 1]
            CONSOLE.error("Choose one of the account numbers shown above.", stream=sys.stdout)

    @staticmethod
    def read_hostname() -> str:
        while True:
            try:
                value = CONSOLE.prompt("Wildcard domain (for example, *.fw.example.com): ").strip().lower().rstrip(".")
            except EOFError:
                raise SetupError("Input ended while reading the wildcard domain.") from None
            if HOST_RE.fullmatch(value):
                return value
            CONSOLE.error("Enter a valid wildcard hostname beginning with *.", stream=sys.stdout)

    @staticmethod
    def yes_no(prompt: str) -> bool:
        try:
            return CONSOLE.prompt(prompt).strip().lower() in ("", "y", "yes")
        except EOFError:
            return False

    def certificate_packs(self) -> List[Any]:
        assert self.zone_id
        return self.all_pages(f"/zones/{self.zone_id}/ssl/certificate_packs?status=all")

    @staticmethod
    def find_certificate(packs: Iterable[Any], hostname: str) -> Optional[Any]:
        lowered = hostname.lower()
        return next((pack for pack in packs if any(str(host).lower() == lowered for host in (pack.get("hosts") or []))), None)

    @staticmethod
    def certificate_active(pack: Any) -> bool:
        return pack.get("status") == "active" or any(cert.get("status") == "active" for cert in (pack.get("certificates") or []))

    @staticmethod
    def certificate_failed(pack: Any) -> bool:
        return pack.get("status") in FAILURE_STATES or any(cert.get("status") in FAILURE_STATES for cert in (pack.get("certificates") or []))

    def dns_by_name(self, name: str) -> List[Any]:
        assert self.zone_id
        return self.all_pages(f"/zones/{self.zone_id}/dns_records?name.exact={urllib.parse.quote(name, safe='')}")

    def upload_worker(self) -> None:
        assert self.account_id and self.worker_name and self.access_token
        boundary = "fw-" + secrets.token_hex(16)
        metadata = self.json_bytes({"main_module": "worker.js", "compatibility_date": WORKER_COMPATIBILITY_DATE})
        code = b'export default {\n  async fetch(request, env, ctx) {\n    return new Response("I\'m here!");\n  }\n};\n'
        chunks = [f"--{boundary}\r\nContent-Disposition: form-data; name=\"metadata\"\r\nContent-Type: application/json\r\n\r\n".encode(), metadata, b"\r\n", f"--{boundary}\r\nContent-Disposition: form-data; name=\"worker.js\"; filename=\"worker.js\"\r\nContent-Type: application/javascript+module\r\n\r\n".encode(), code, b"\r\n", f"--{boundary}--\r\n".encode()]
        _, raw = self.request(f"{API_BASE}/accounts/{self.account_id}/workers/scripts/{self.worker_name}", "PUT", b"".join(chunks), {"Authorization": f"Bearer {self.access_token}", "Content-Type": f"multipart/form-data; boundary={boundary}"})
        try:
            response = json.loads(raw)
        except json.JSONDecodeError:
            raise SetupError("Worker upload returned invalid JSON.") from None
        if response.get("success") is not True:
            raise SetupError("Worker upload failed: " + self.error_message(response, "Cloudflare rejected the Worker script."))

    def worker_workaround(self, hostname: str, zone_name: str, setup_hex: str) -> Any:
        assert self.account_id and self.zone_id
        custom = hostname[2:]
        if self.dns_by_name(custom):
            raise SetupError(f"A DNS record already exists for {custom}, which conflicts with the Worker certificate workaround.")
        CONSOLE.line()
        CONSOLE.detail("The domain ", hostname, end="")
        CONSOLE.line(" requires Cloudflare Advanced Certificate Management, which can cost around $10/month.\n")
        CONSOLE.line("FW can try a Cloudflare Workers provisioning workaround to generate the required wildcard certificate. This behavior is not guaranteed by Cloudflare. FW will verify that the requested certificate was created and becomes ready.\n")
        CONSOLE.line("If you continue, you'll be redirected to Cloudflare again to approve the additional permission.\n")
        if not self.yes_no("Continue with the Worker approach? (Y/n): "):
            raise SetupError("Setup stopped. Create the required wildcard certificate with Advanced Certificate Manager, then run setup again.")
        self.oauth_login(BASE_SCOPES + [WORKERS_SCOPE])
        self.status("Checking Worker scopes...")
        workers = self.all_pages(f"/accounts/{self.account_id}/workers/scripts")
        domains = self.all_pages(f"/accounts/{self.account_id}/workers/domains")
        self.ok()
        if any(str(item.get("hostname") or "").lower() == custom.lower() for item in domains):
            raise SetupError(f"A Worker custom domain already exists for {custom}. FW will not overwrite it.")
        self.worker_name = f"proxy-fw-{setup_hex}"
        if any(item.get("id") == self.worker_name or item.get("name") == self.worker_name for item in workers):
            raise SetupError(f"The generated Worker name already exists: {self.worker_name}. Run setup again.")
        self.status(f"Creating Worker {self.worker_name}...")
        self.upload_worker()
        self.worker_created = True
        self.ok()
        self.status(f"Attaching Worker domain {custom}...")
        domain = self.cf_api("PUT", f"/accounts/{self.account_id}/workers/domains", {"hostname": custom, "service": self.worker_name, "zone_id": self.zone_id, "zone_name": zone_name})
        self.worker_domain_id = str(domain.get("id") or "")
        if not self.worker_domain_id:
            raise SetupError("Cloudflare returned no Worker domain ID.")
        self.ok()
        for attempt in range(1, WORKER_CERTIFICATE_ATTEMPTS + 1):
            self.status(f"Checking domain ACM ({attempt}/{WORKER_CERTIFICATE_ATTEMPTS})...")
            time.sleep(CERTIFICATE_POLL_SECONDS)
            pack = self.find_certificate(self.certificate_packs(), hostname)
            if pack:
                if self.certificate_failed(pack):
                    raise SetupError(f"The Worker approach created a certificate pack in a failed state: {pack.get('status')}.")
                self.ok()
                CONSOLE.line()
                CONSOLE.success("ACM created successfully via Worker approach.")
                CONSOLE.detail("ACM status: ", str(pack.get("status")).upper())
                CONSOLE.line()
                return pack
            self.ok("NOT READY")
        raise SetupError("The Worker domain was created, but Cloudflare did not create the requested wildcard certificate. Use Advanced Certificate Manager and try again.")

    def start_test_server(self) -> int:
        class TestServer(http.server.HTTPServer):
            def get_request(inner_self) -> Tuple[socket.socket, Any]:
                connection, address = super().get_request()
                connection.settimeout(10)
                return connection, address

        class Handler(http.server.BaseHTTPRequestHandler):
            def log_message(self, _format: str, *args: Any) -> None:
                return
            def do_GET(self) -> None:
                body = b"ok"
                self.send_response(200)
                self.send_header("Content-Type", "text/plain; charset=utf-8")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
        self.test_server = TestServer(("127.0.0.1", 0), Handler)
        self.test_thread = threading.Thread(target=self.test_server.serve_forever, daemon=True)
        self.test_thread.start()
        port = self.test_server.server_port
        status, body = self.request(f"http://127.0.0.1:{port}/", timeout=5, max_bytes=32)
        if status != 200 or body != b"ok":
            raise SetupError("The local test server returned an unexpected response.")
        return port

    def stop_processes(self) -> None:
        if self.fw_process and self.fw_process.poll() is None:
            with contextlib.suppress(OSError):
                os.killpg(self.fw_process.pid, signal.SIGTERM)
            try:
                self.fw_process.wait(timeout=5)
            except (OSError, subprocess.TimeoutExpired):
                with contextlib.suppress(OSError):
                    os.killpg(self.fw_process.pid, signal.SIGKILL)
                with contextlib.suppress(OSError, subprocess.TimeoutExpired):
                    self.fw_process.wait(timeout=2)
        self.fw_process = None
        if self.test_server:
            with contextlib.suppress(Exception):
                self.test_server.shutdown()
            with contextlib.suppress(Exception):
                self.test_server.server_close()
            if self.test_thread:
                with contextlib.suppress(Exception):
                    self.test_thread.join(timeout=5)
        self.test_server = None
        self.test_thread = None

    def wait_tunnel(self) -> None:
        assert self.account_id and self.tunnel_id
        for _ in range(20):
            tunnel = self.cf_api("GET", f"/accounts/{self.account_id}/cfd_tunnel/{self.tunnel_id}")
            if tunnel.get("status") in ("healthy", "degraded"):
                return
            time.sleep(3)
        raise SetupError("The Cloudflare Tunnel did not connect in time.")

    def wait_certificate(self, hostname: str) -> None:
        for attempt in range(1, CERTIFICATE_POLL_ATTEMPTS + 1):
            self.status(f"Waiting for certificate ({attempt}/{CERTIFICATE_POLL_ATTEMPTS})...")
            time.sleep(CERTIFICATE_POLL_SECONDS)
            pack = self.find_certificate(self.certificate_packs(), hostname)
            if pack and self.certificate_active(pack):
                self.ok()
                return
            if pack and self.certificate_failed(pack):
                raise SetupError(f"Certificate provisioning failed with status '{pack.get('status')}'.")
            self.ok("PENDING")
        raise SetupError("The wildcard certificate was not active after 10 minutes.")

    def test_https(self, hostname: str) -> None:
        self.status(f"Resolving {hostname} with Cloudflare DNS...")
        _, doh_raw = self.request(f"https://cloudflare-dns.com/dns-query?name={urllib.parse.quote(hostname, safe='')}&type=A", headers={"Accept": "application/dns-json"}, timeout=30)
        try:
            response = json.loads(doh_raw)
        except json.JSONDecodeError:
            raise SetupError("Cloudflare DoH returned an invalid response.") from None
        if int(response.get("Status", -1)) != 0:
            raise SetupError(f"Cloudflare DoH returned status {response.get('Status')} for {hostname}.")
        address = next((answer.get("data") for answer in response.get("Answer", []) if answer.get("type") == 1), None)
        if not address:
            raise SetupError(f"Cloudflare DoH returned no A record for {hostname}.")
        self.ok()
        self.status(f"Testing https://{hostname}/...")
        raw = socket.create_connection((address, 443), timeout=10)
        secure: Optional[ssl.SSLSocket] = None
        try:
            secure = ssl.create_default_context().wrap_socket(raw, server_hostname=hostname)
            secure.settimeout(30)
            request = f"GET / HTTP/1.1\r\nHost: {hostname}\r\nUser-Agent: FW-Setup/{CLOUDFLARED_VERSION}\r\nConnection: close\r\n\r\n".encode("ascii")
            secure.sendall(request)
            response = http.client.HTTPResponse(secure)
            response.begin()
            response.read(1024 * 1024)
            if not 200 <= response.status < 400:
                raise SetupError(f"Tunnel HTTPS test returned HTTP {response.status}.")
        except (OSError, ssl.SSLError, http.client.HTTPException) as error:
            raise SetupError(f"Tunnel test failed for https://{hostname}/: {error}") from None
        finally:
            with contextlib.suppress(Exception):
                if secure is not None:
                    secure.close()
                else:
                    raw.close()
        self.ok()
        CONSOLE.line()
        CONSOLE.success("Tunnel test completed successfully.")
        CONSOLE.line()

    def rollback(self) -> None:
        if self.committed:
            return
        self.stop_processes()
        if self.access_token:
            operations = []
            if self.dns_record_id and self.zone_id:
                operations.append(f"/zones/{self.zone_id}/dns_records/{self.dns_record_id}")
            if self.tunnel_id and self.account_id:
                operations.append(f"/accounts/{self.account_id}/cfd_tunnel/{self.tunnel_id}")
            if self.worker_domain_id and self.account_id:
                operations.append(f"/accounts/{self.account_id}/workers/domains/{self.worker_domain_id}")
            if self.worker_created and self.worker_name and self.account_id:
                operations.append(f"/accounts/{self.account_id}/workers/scripts/{self.worker_name}")
            for path in operations:
                try:
                    self.cf_api("DELETE", path)
                except Exception:
                    pass
        for path in reversed(self.created_files + self.staged_files):
            with contextlib.suppress(OSError):
                path.unlink()

    def recover(self) -> None:
        try:
            self.rollback()
        except BaseException:
            pass
        try:
            self.revoke_tokens()
        except BaseException:
            pass

    def run(self) -> None:
        self.validate_release_configuration()
        self.validate_paths()
        self.artifact()
        setup_hex = secrets.token_hex(2)
        CONSOLE.line()
        self.oauth_login(BASE_SCOPES)
        self.status("Checking scopes...")
        accounts = self.all_pages("/accounts")
        self.ok()
        account = self.choose(accounts, "name")
        self.account_id = str(account["id"])
        CONSOLE.line()
        CONSOLE.detail("Cloudflare account: ", account.get("name", ""))
        hostname = self.read_hostname()
        self.status("Checking domain availability...")
        zones = [zone for zone in self.all_pages(f"/zones?account.id={urllib.parse.quote(self.account_id, safe='')}") if zone.get("status") == "active"]
        plain = hostname[2:]
        matches = [zone for zone in zones if plain == zone.get("name") or plain.endswith("." + str(zone.get("name")))]
        if not matches:
            raise SetupError("No active Cloudflare zone in the selected account matches that domain.")
        zone = max(matches, key=lambda item: len(str(item.get("name"))))
        self.zone_id = str(zone["id"])
        zone_name = str(zone["name"])
        relative = plain[:-(len(zone_name) + 1)] if plain != zone_name else ""
        relative_labels = len(relative.split(".")) if relative else 0
        if relative_labels > 2:
            raise SetupError("The wildcard domain may be at most three levels deep relative to its Cloudflare zone.")
        if self.dns_by_name(hostname):
            raise SetupError(f"A DNS record already exists for {hostname}. FW will not overwrite it.")
        self.ok()
        self.status("Checking domain ACM...")
        certificate = self.find_certificate(self.certificate_packs(), hostname)
        if certificate and self.certificate_failed(certificate):
            raise SetupError(f"The existing certificate for {hostname} is in a failed state: {certificate.get('status')}.")
        self.ok(str(certificate.get("status")).upper() if certificate else "NOT FOUND")
        if relative_labels in (1, 2) and not certificate:
            certificate = self.worker_workaround(hostname, zone_name, setup_hex)
        self.install_cloudflared()
        assert self.cf_dir and self.cloudflared_path and self.config_path
        tunnels = self.all_pages(f"/accounts/{self.account_id}/cfd_tunnel?is_deleted=false")
        tunnel_name = f"tunnel-fw-{setup_hex}"
        if any(item.get("name") == tunnel_name for item in tunnels):
            raise SetupError(f"The generated tunnel name already exists: {tunnel_name}. Run setup again.")
        tunnel_secret = base64.b64encode(secrets.token_bytes(32)).decode("ascii")
        self.status("Creating tunnel...")
        tunnel = self.cf_api("POST", f"/accounts/{self.account_id}/cfd_tunnel", {"name": tunnel_name, "config_src": "local", "tunnel_secret": tunnel_secret})
        self.tunnel_id = str(tunnel.get("id") or "")
        if not self.tunnel_id:
            raise SetupError("Cloudflare reported tunnel creation success but returned no tunnel ID.")
        self.ok()
        credentials_path = self.cf_dir / f"credentials-{self.tunnel_id}.json"
        self.write_atomic(credentials_path, self.json_bytes({"AccountTag": self.account_id, "TunnelSecret": tunnel_secret, "TunnelID": self.tunnel_id}), 0o600)
        tunnel_secret = ""
        self.status("Creating tunnel DNS record...")
        dns = self.cf_api("POST", f"/zones/{self.zone_id}/dns_records", {"type": "CNAME", "name": hostname, "content": f"{self.tunnel_id}.cfargotunnel.com", "proxied": True, "ttl": 1})
        self.dns_record_id = str(dns.get("id") or "")
        if not self.dns_record_id:
            raise SetupError("Cloudflare reported DNS record creation success but returned no record ID.")
        self.ok()
        self.status("Starting local test server...")
        test_port = self.start_test_server()
        self.ok()
        test_slug = f"setup-{setup_hex}"
        test_hostname = f"{test_slug}.{hostname[2:]}"
        yaml = (f"tunnel: {json.dumps(self.tunnel_id)}\ncredentials-file: {json.dumps(str(credentials_path))}\n\ningress:\n  - hostname: {json.dumps(hostname)}\n    service: {json.dumps(f'http://127.0.0.1:{test_port}')}\n  - service: \"http_status:404\"\n").encode("utf-8")
        self.write_atomic(self.config_path, yaml, 0o600)
        self.status("Validating tunnel configuration...")
        validation = subprocess.run([str(self.cloudflared_path), "tunnel", "--config", str(self.config_path), "ingress", "validate"], stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, timeout=30, check=False)
        if validation.returncode != 0:
            raise SetupError("cloudflared rejected the generated config.yml.")
        self.ok()
        CONSOLE.line()
        CONSOLE.success("Tunnel & DNS created successfully.")
        CONSOLE.line()
        self.status(f"Starting FW on port {test_port} with slug {test_slug}...")
        self.fw_process = subprocess.Popen([str(self.fw_path), "start", str(test_port), "--slug", test_slug], cwd=str(self.fw_path.parent), stdin=subprocess.DEVNULL, stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL, start_new_session=True)
        time.sleep(3)
        if self.fw_process.poll() is not None:
            raise SetupError(f"FW start exited unexpectedly with code {self.fw_process.returncode}.")
        self.ok()
        self.status("Waiting for tunnel connection...")
        self.wait_tunnel()
        self.ok()
        self.wait_certificate(hostname)
        self.test_https(test_hostname)
        self.status("Finishing setup...")
        self.stop_processes()
        self.committed = True
        self.revoke_tokens()
        self.ok()
        CONSOLE.line()
        CONSOLE.success("FW setup completed successfully.")
        CONSOLE.detail("Tunnel: ", tunnel_name)
        CONSOLE.detail("Domain: ", hostname)
        CONSOLE.detail("Configuration: ", self.config_path)
        CONSOLE.line()
        try:
            CONSOLE.prompt("Press Enter to close.")
        except EOFError:
            pass


def parse_args() -> argparse.Namespace:
    parser = argparse.ArgumentParser(prog="fw-setup.sh")
    parser.add_argument("--fw-path", type=Path, required=True)
    parser.add_argument("--portable", action="store_true")
    return parser.parse_args()

def main() -> int:
    args = parse_args()
    setup = Setup(args.fw_path, args.portable)
    def interrupt(_signum: int, _frame: Any) -> None:
        raise KeyboardInterrupt
    for signal_name in ("SIGTERM", "SIGHUP"):
        if hasattr(signal, signal_name):
            signal.signal(getattr(signal, signal_name), interrupt)
    try:
        setup.run()
        return 0
    except KeyboardInterrupt:
        with contextlib.suppress(Exception):
            CONSOLE.fail()
            CONSOLE.error("Setup was interrupted; rolling back.")
        setup.recover()
        return 130
    except Exception as error:
        with contextlib.suppress(Exception):
            CONSOLE.fail()
        setup.recover()
        with contextlib.suppress(Exception):
            CONSOLE.error(error)
        return 1
    finally:
        setup.cleanup_local()

if __name__ == "__main__":
    raise SystemExit(main())
FW_SETUP_PYTHON
exit $?
