// Explicit test configuration prevents compiled-in checkout defaults from reading
// the developer's model catalog, dotenv secrets or user configuration directory.
import fs from "node:fs";
import path from "node:path";

export function isolatedModelFixture(root) {
  const catalog = path.join(root, "fixture-models.yaml");
  const dotenv = path.join(root, "fixture-model.env");
  const home = path.join(root, "fixture-kageha-home");
  fs.mkdirSync(home, { recursive: true });
  if (!fs.existsSync(catalog))
    fs.writeFileSync(catalog, "providers: {}\n", { mode: 0o600 });
  if (!fs.existsSync(dotenv)) fs.writeFileSync(dotenv, "", { mode: 0o600 });
  return {
    KAGEHA_MODELS_FILE: catalog,
    KAGEHA_MODEL_ENV_FILE: dotenv,
    KAGEHA_HOME: home,
  };
}
