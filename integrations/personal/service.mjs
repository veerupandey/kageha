// One release and three supervised processes on macOS or Linux. No secrets in service definitions.
import fs from "node:fs";
import path from "node:path";
import os from "node:os";
import { fileURLToPath } from "node:url";
import { spawn, spawnSync, execFileSync } from "node:child_process";
const here = path.dirname(fileURLToPath(import.meta.url));
const repo = path.resolve(here, "../..");
const [mode, dirArg] = process.argv.slice(2);
const dir = path.resolve(dirArg || path.join(os.homedir(), ".kageha-personal"));
const config = JSON.parse(
  fs.readFileSync(path.join(dir, "personal.json"), "utf8"),
);
const names = ["browser", "backend", "gateway"];
if (config.quickShareTunnel) names.push("share");
const port = new URL(config.backend).port;
const origin = "http://127.0.0.1:" + config.port;
const chromium =
  config.chromium ||
  (process.platform === "darwin"
    ? "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome"
    : "/usr/bin/chromium");
const bins = path.join(dir, "bin");
const program = {
  browser: [
    path.join(bins, "kageha-browser"),
    "--state-dir",
    path.join(dir, "browser-standalone"),
    "--instance-id",
    "personal",
    "--application-id",
    "kageha-personal",
    "--chromium",
    chromium,
    "--standalone",
    "on",
  ],
  backend: [
    path.join(bins, "kageha-loop"),
    "serve",
    path.join(dir, "workspace"),
    path.join(dir, "state"),
    "127.0.0.1:" + port,
  ],
  share: [process.execPath,path.join(here,"share-tunnel.mjs"),dir],
  gateway: [
    process.execPath,
    path.join(here, "server.mjs"),
    path.join(dir, "personal.json"),
  ],
};
process.umask(0o077);
fs.mkdirSync(path.join(dir, "browser-standalone"), {
  recursive: true,
  mode: 0o700,
});
if (names.includes(mode)) {
  const env = {
    ...process.env,
    PATH: [
      path.dirname(process.execPath),
      bins,
      "/usr/local/bin",
      "/opt/homebrew/bin",
      "/usr/bin",
      "/bin",
      process.env.PATH,
    ].join(":"),
    KAGEHA_HOME: path.join(dir, "home"),
    KAGEHA_BROWSER_STATE_DIR: path.join(dir, "browser-standalone"),
    KAGEHA_WEBUI_DIST: config.webDist || path.join(repo, "frontend/dist"),
    KAGEHA_RESOURCE_DIR: path.join(repo, "resources"),
    KAGEHA_MODELS_FILE: path.join(repo, "resources/models.yaml"),
    KAGEHA_PERSONAL_MACHINE: "docker",
    KAGEHA_CREDENTIAL_KEY_FILE: path.join(dir, "vault.key"),
    KAGEHA_SERVER_TOKEN: config.backendToken,
    KAGEHA_PERSONAL_TOKEN: config.toolToken,
    KAGEHA_PERSONAL_GATEWAY: origin,
  };
  if (config.model && config.modelUrl)
    Object.assign(env, {
      KAGEHA_MODEL: config.model,
      KAGEHA_PROVIDER_URL: config.modelUrl,
    });
  if (mode === "backend" && config.localModel) {
    const { cli, key, contextLength } = config.localModel;
    const endpoint = new URL(config.modelUrl);
    if (
      endpoint.hostname !== "127.0.0.1" ||
      endpoint.protocol !== "http:" ||
      !config.model ||
      !key ||
      !path.isAbsolute(cli) ||
      !Number.isInteger(contextLength)
    )
      throw Error("Invalid local model configuration");
    try {
      const r = await fetch(new URL("/v1/models", endpoint), {
        signal: AbortSignal.timeout(3000),
      });
      if (!r.ok) throw Error("Model endpoint unavailable");
    } catch {
      execFileSync(cli, ["server", "start", "--port", endpoint.port], {
        timeout: 60000,
        stdio: "inherit",
      });
    }
    const models = JSON.parse(
      execFileSync(cli, ["ps", "--json"], { encoding: "utf8", timeout: 10000 }),
    );
    if (!models.some((m) => m.identifier === config.model))
      execFileSync(
        cli,
        [
          "load",
          key,
          "--identifier",
          config.model,
          "--context-length",
          String(contextLength),
        ],
        { timeout: 180000, stdio: "inherit" },
      );
  }
  // Transport processes need only their own configuration and state.
  const [bin, ...args] = program[mode];
  const child = spawn(bin, args, {
    cwd: dir,
    env: mode === "backend" ? env : { ...process.env, PATH: env.PATH },
    stdio: "inherit",
  });
  for (const sig of ["SIGTERM", "SIGINT"])
    process.on(sig, () => child.kill(sig));
  child.on("error", (e) => {
    console.error(e.message);
    process.exit(1);
  });
  child.on("exit", (code) => process.exit(code ?? 1));
} else if (mode === "install" || mode === "install-share") {
  fs.mkdirSync(bins, { recursive: true });
  if (mode === "install") for (const name of [
    "kageha-loop",
    "kageha-browser",
    "kageha-shell-service",
  ]) {
    const source = path.join(
      process.env.KAGEHA_RELEASE_DIR || path.join(repo, "target/release"),
      name,
    );
    fs.copyFileSync(source, path.join(bins, name + ".new"));
    fs.chmodSync(path.join(bins, name + ".new"), 0o700);
    fs.renameSync(path.join(bins, name + ".new"), path.join(bins, name));
  }
  const xml = (v) =>
    String(v)
      .replaceAll("&", "&amp;")
      .replaceAll("<", "&lt;")
      .replaceAll(">", "&gt;");
  for (const name of mode === "install-share" ? ["share"] : names) {
    const args = [process.execPath, fileURLToPath(import.meta.url), name, dir];
    const label = "org.kageha.personal." + name;
    if (process.platform === "darwin") {
      const folder = path.join(os.homedir(), "Library/LaunchAgents");
      fs.mkdirSync(folder, { recursive: true });
      const file = path.join(folder, label + ".plist");
      fs.writeFileSync(
        file,
        '<?xml version="1.0"?><plist version="1.0"><dict><key>Label</key><string>' +
          label +
          "</string><key>ProgramArguments</key><array>" +
          args.map((s) => "<string>" + xml(s) + "</string>").join("") +
          "</array><key>RunAtLoad</key><true/><key>KeepAlive</key><true/><key>ThrottleInterval</key><integer>5</integer><key>StandardOutPath</key><string>" +
          xml(path.join(dir, "logs", name + ".log")) +
          "</string><key>StandardErrorPath</key><string>" +
          xml(path.join(dir, "logs", name + ".log")) +
          "</string></dict></plist>",
        { mode: 0o600 },
      );
      const domain = "gui/" + process.getuid();
      if (
        spawnSync("launchctl", ["print", domain + "/" + label], {
          stdio: "ignore",
        }).status !== 0
      )
        execFileSync("launchctl", ["bootstrap", domain, file]);
    } else if (process.platform === "linux") {
      const folder = path.join(os.homedir(), ".config/systemd/user");
      fs.mkdirSync(folder, { recursive: true });
      const unit = label + ".service";
      const quote = (s) =>
        '"' +
        s
          .replaceAll("\\", "\\\\")
          .replaceAll('"', '\\"')
          .replaceAll("%", "%%")
          .replaceAll("$", "$$") +
        '"';
      fs.writeFileSync(
        path.join(folder, unit),
        "[Unit]\nDescription=Kageha Personal " +
          name +
          "\n[Service]\nExecStart=" +
          args.map(quote).join(" ") +
          "\nRestart=always\nRestartSec=3\nUMask=0077\n[Install]\nWantedBy=default.target\n",
      );
      execFileSync("systemctl", ["--user", "daemon-reload"]);
      execFileSync("systemctl", ["--user", "enable", "--now", unit]);
    } else throw Error("Use macOS or Linux");
  }
  console.log(
    "Personal services installed. Existing processes pick up staged changes on their next restart.",
  );
} else throw Error("Usage: node service.mjs install [instance-directory]");
