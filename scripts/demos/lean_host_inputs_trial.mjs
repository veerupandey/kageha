// Live Full-access regression. All clips and the host home belong to this trial.
import assert from "node:assert/strict";
import { execFileSync } from "node:child_process";
import fs from "node:fs";
import path from "node:path";
import { startLiveTrial } from "./lean_live_trial_host.mjs";

const trial = await startLiveTrial({ maxCalls: 24, seconds: 160 });
try {
  const { session, workspace } = await trial.session();
  const downloads = path.join(path.dirname(trial.state), "Downloads");
  fs.mkdirSync(downloads);
  const clips = [
    { name: "green", color: "008000", rgb: [0, 128, 0] },
    { name: "white", color: "ffffff", rgb: [255, 255, 255] },
    { name: "blue", color: "0000ff", rgb: [0, 0, 255] },
  ];
  for (const { name, color } of clips) {
    execFileSync("ffmpeg", ["-v", "error", "-f", "lavfi", "-i",
      `color=c=0x${color}:s=96x64:r=10:d=1`, "-c:v", "mpeg4", path.join(downloads, name + ".mov")]);
  }
  await trial.send(session, "Join my three local videos ~/Downloads/green.mov, ~/Downloads/white.mov and ~/Downloads/blue.mov in that order into outputs/sequence.mp4. Keep each entire clip and preserve its colors. Check the result.");
  const tools = trial.calls.flatMap(call => call.tool_calls.map(tool => tool.name));
  const output = path.join(workspace, "outputs", "sequence.mp4");
  console.log(JSON.stringify({ model: trial.model, tools, outputExists: fs.existsSync(output) }));
  assert.ok(fs.existsSync(output), "The agent did not deliver the file");
  const duration = Number(execFileSync("ffprobe", ["-v", "error", "-show_entries", "format=duration", "-of", "csv=p=0", output]));
  const frames = clips.map((_, i) => Array.from(execFileSync("ffmpeg", ["-v", "error", "-ss", String(i + 0.5),
    "-i", output, "-vframes", "1", "-vf", "scale=1:1", "-pix_fmt", "rgb24", "-f", "rawvideo", "-"])));
  console.log(JSON.stringify({ duration, frames }));
  assert.ok(Math.abs(duration - 3) < 0.15, "Unexpected output duration");
  clips.forEach(({ name, rgb }, i) => {
    assert.equal(frames[i].length, 3);
    assert.ok(frames[i].every((channel, j) => Math.abs(channel - rgb[j]) < 12), "Unexpected color for " + name);
  });
} finally {
  await trial.close();
}
