const { app, BrowserWindow, WebContentsView, ipcMain } = require("electron");
const { spawn } = require("child_process");
const http = require("http");
const net = require("net");
const path = require("path");

const repoRoot = path.resolve(__dirname, "..");

function freePort() {
  return new Promise((resolve, reject) => {
    const sock = net.createServer();
    sock.listen(0, "127.0.0.1", () => {
      const addr = sock.address();
      const port = typeof addr === "object" && addr ? addr.port : 0;
      sock.close((err) => (err ? reject(err) : resolve(port)));
    });
    sock.on("error", reject);
  });
}

function waitForHttp(url, timeoutMs) {
  const deadline = Date.now() + timeoutMs;
  return new Promise((resolve, reject) => {
    const attempt = () => {
      const req = http.get(url, (res) => {
        res.resume();
        if ((res.statusCode || 500) < 500) {
          resolve();
          return;
        }
        retry();
      });
      req.on("error", retry);
      req.setTimeout(1000, () => {
        req.destroy();
        retry();
      });
    };
    const retry = () => {
      if (Date.now() > deadline) {
        reject(new Error(`WebUI did not start at ${url}`));
        return;
      }
      setTimeout(attempt, 250);
    };
    attempt();
  });
}

function spawnWebui(port, cdpUrl) {
  const bin = process.env.KAGEHA_BIN;
  const args = bin
    ? [bin, "webui", "--host", "127.0.0.1", "--port", String(port)]
    : ["uv", "run", "kageha", "webui", "--host", "127.0.0.1", "--port", String(port)];
  const child = spawn(args[0], args.slice(1), {
    cwd: repoRoot,
    env: {
      ...process.env,
      KAGEHA_EMBEDDED_CDP: cdpUrl,
      KAGEHA_WEBUI_PORT: String(port),
      PYTHONUNBUFFERED: "1",
    },
    stdio: ["ignore", "pipe", "pipe"],
  });
  child.stdout.on("data", (chunk) => process.stdout.write(chunk));
  child.stderr.on("data", (chunk) => process.stderr.write(chunk));
  return child;
}

function portFree(port) {
  return new Promise((resolve) => {
    const sock = net.createServer();
    sock.once("error", () => resolve(false));
    sock.listen(port, "127.0.0.1", () => {
      sock.close(() => resolve(true));
    });
  });
}

let guestView = null;
let mainWindow = null;
let webui = null;
let guestInteractive = false;

function applyGuestPointerLock() {
  if (!guestView) return;
  const script = guestInteractive
    ? "document.documentElement.style.pointerEvents = '';"
    : "document.documentElement.style.pointerEvents = 'none';";
  guestView.webContents.executeJavaScript(script).catch(() => {});
  if (!guestInteractive && mainWindow && !mainWindow.isDestroyed()) {
    mainWindow.webContents.focus();
  }
}

async function createWindow(uiPort) {
  const win = new BrowserWindow({
    width: 1440,
    height: 900,
    show: true,
    webPreferences: {
      preload: path.join(__dirname, "preload.js"),
      contextIsolation: true,
      nodeIntegration: false,
    },
  });
  mainWindow = win;
  guestView = new WebContentsView({
    webPreferences: {
      preload: path.join(__dirname, "preload-guest.js"),
      contextIsolation: true,
      nodeIntegration: false,
      partition: "persist:kageha",
    },
  });
  win.contentView.addChildView(guestView);
  guestView.setBounds({ x: 0, y: 0, width: 1, height: 1 });
  guestView.webContents.on("did-finish-load", applyGuestPointerLock);
  guestView.webContents.on("did-navigate", applyGuestPointerLock);
  await guestView.webContents.loadFile(path.join(__dirname, "start.html"));
  await win.loadURL(`http://127.0.0.1:${uiPort}/`);
  win.on("closed", () => {
    guestView = null;
    mainWindow = null;
  });
  return win;
}

ipcMain.on("kageha:guest-bounds", (_event, bounds) => {
  if (!guestView || !bounds) return;
  const width = Math.max(0, Math.round(bounds.width || 0));
  const height = Math.max(0, Math.round(bounds.height || 0));
  guestView.setBounds({
    x: Math.round(bounds.x || 0),
    y: Math.round(bounds.y || 0),
    width,
    height,
  });
});

ipcMain.on("kageha:guest-interactive", (_event, interactive) => {
  guestInteractive = Boolean(interactive);
  applyGuestPointerLock();
  if (guestInteractive && guestView) {
    guestView.webContents.focus();
  }
});

async function boot() {
  const uiPort = Number(process.env.KAGEHA_WEBUI_PORT) || (await freePort());
  const requested = Number(process.env.KAGEHA_EMBEDDED_DEBUG_PORT || "9334");
  const cdpPort = (await portFree(requested)) ? requested : await freePort();
  app.commandLine.appendSwitch("remote-debugging-port", String(cdpPort));
  app.commandLine.appendSwitch("remote-allow-origins", "*");
  await app.whenReady();
  webui = spawnWebui(uiPort, `http://127.0.0.1:${cdpPort}`);
  await waitForHttp(`http://127.0.0.1:${uiPort}/`, 45000);
  await createWindow(uiPort);
}

boot().catch((err) => {
  console.error(err);
  if (webui && !webui.killed) webui.kill();
  app.exit(1);
});

app.on("window-all-closed", () => {
  if (webui && !webui.killed) webui.kill();
  app.quit();
});

app.on("before-quit", () => {
  if (webui && !webui.killed) webui.kill();
});
