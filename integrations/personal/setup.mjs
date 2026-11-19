import fs from "node:fs";
import path from "node:path";
import { randomBytes } from "node:crypto";
import { createInterface } from "node:readline/promises";
import { stdin, stdout } from "node:process";

process.umask(0o077);
const dest = path.resolve(process.argv[2] || "personal-instance");
if (fs.existsSync(dest))
  throw Error("Choose a new directory for this private instance");
const local = process.argv.includes("--local");
let url = "",
  phone = "",
  number = "",
  mac = "";
if (!local) {
  const rl = createInterface({ input: stdin, output: stdout });
  try {
    url = (
      await rl.question("Public HTTPS origin for incoming messages and calls: ")
    ).trim();
    phone = (
      await rl.question("Owner phone number in international format: ")
    ).trim();
    number = (
      await rl.question("Dedicated Twilio number in international format: ")
    ).trim();
    mac = (await rl.question("Owner iMessage address: ")).trim();
  } finally {
    rl.close();
  }
  if (
    !/^https:\/\/[^/?#]+$/.test(url) ||
    ![phone, number].every((x) => /^\+[1-9]\d{6,14}$/.test(x)) ||
    !mac ||
    phone === number
  )
    throw Error("Invalid origin, owner or assistant number");
}
fs.mkdirSync(dest, { mode: 0o700 });
const key = () => randomBytes(32).toString("hex");
const config = {
  publicUrl: url,
  backend: "http://127.0.0.1:8889",
  backendToken: key(),
  toolToken: key(),
  bridgeToken: key(),
  database: path.join(dest, "delivery.sqlite"),
  port: 8891,
  ownerPhone: phone,
  number,
  wa: number ? "whatsapp:" + number : "",
  sid: "",
  auth: "",
  voicePin: String(
    (await import("node:crypto")).randomInt(10000000, 100000000),
  ),
  template: "",
  macOwner: mac,
  defaultChannel: local ? "local" : "sms",
  timezone: Intl.DateTimeFormat().resolvedOptions().timeZone,
  digestHour: 18,
  monitorInboxAndCalendar: false,
  callRules: [],
  watches: [],
};
function save(name, data) {
  fs.writeFileSync(path.join(dest, name), data, { mode: 0o600, flag: "wx" });
}
save("personal.json", JSON.stringify(config, null, 2) + "\n");
save(
  "bridge.json",
  JSON.stringify(
    {
      publicUrl: url || "http://127.0.0.1:8891",
      owner: mac,
      token: config.bridgeToken,
      chatId: 0,
      database: path.join(dest, "mac-delivery.sqlite"),
    },
    null,
    2,
  ) + "\n",
);
save("vault.key", randomBytes(32));
save("web.token", config.backendToken);
fs.mkdirSync(path.join(dest, "workspace"));
fs.mkdirSync(path.join(dest, "home"));
fs.mkdirSync(path.join(dest, "browser-standalone"));
fs.mkdirSync(path.join(dest, "logs"));
save(
  "backend.env",
  [
    "KAGEHA_SERVER_TOKEN=" + config.backendToken,
    "KAGEHA_PERSONAL_TOKEN=" + config.toolToken,
    "KAGEHA_PERSONAL_GATEWAY=http://127.0.0.1:8891",
    "KAGEHA_PERSONAL_MACHINE=docker",
    "KAGEHA_BROWSER_STATE_DIR=" +
      JSON.stringify(path.join(dest, "browser-standalone")),
    "KAGEHA_HOME=" + JSON.stringify(path.join(dest, "home")),
    "KAGEHA_CREDENTIAL_KEY_FILE=" +
      JSON.stringify(path.join(dest, "vault.key")),
  ].join("\n") + "\n",
);
console.log(
  "Private configuration created in " +
    dest +
    ". Add the Twilio credentials to personal.json and pair the Mac using bridge.json.",
);
