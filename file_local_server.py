from http.server import ThreadingHTTPServer, SimpleHTTPRequestHandler
from pathlib import Path
import os

HOST = "0.0.0.0"
PORT = 8000

BASE_DIR = Path(__file__).resolve().parent
STATIC_DIR = BASE_DIR / "storage" / "files"


class StaticFileHandler(SimpleHTTPRequestHandler):
    def __init__(self, *args, **kwargs):
        super().__init__(*args, directory=str(STATIC_DIR), **kwargs)

    def end_headers(self):
        # 按需加一点基础响应头
        self.send_header("Cache-Control", "no-store")
        super().end_headers()


def main():
    if not STATIC_DIR.exists():
        raise FileNotFoundError(f"目录不存在: {STATIC_DIR}")

    os.chdir(STATIC_DIR)

    server = ThreadingHTTPServer((HOST, PORT), StaticFileHandler)
    print(f"静态文件服务已启动: http://{HOST}:{PORT}")
    print(f"服务目录: {STATIC_DIR}")
    server.serve_forever()


if __name__ == "__main__":
    main()
