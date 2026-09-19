import assert from "node:assert/strict";
import { spawn } from "node:child_process";
import { once } from "node:events";
import * as fs from "node:fs";
import { tmpdir } from "node:os";
import { dirname, join } from "node:path";
import { fileURLToPath } from "node:url";

const pkg = process.env.PI_AUTH_TEST_PACKAGE;
assert.ok(pkg, "PI_AUTH_TEST_PACKAGE must name the packaged Pi module root");
const { AuthStorage, FileAuthStorageBackend } = await import(`${pkg}/dist/core/auth-storage.js`);
const { createModels } = await import(`${pkg}/node_modules/@earendil-works/pi-ai/dist/models.js`);
const { openaiCodexProvider } = await import(`${pkg}/node_modules/@earendil-works/pi-ai/dist/providers/openai-codex.js`);
const expired = { type: "oauth", access: "fixture-expired", refresh: "fixture-refresh", expires: 1 };
const jwt = `fixture.${Buffer.from(JSON.stringify({ "https://api.openai.com/auth": { chatgpt_account_id: "fixture-account" } })).toString("base64url")}.fixture`;

function waitFor(path) {
  return new Promise((resolve, reject) => {
    let watcher, deadline, poll;
    const finish = (error) => {
      watcher?.close();
      clearTimeout(deadline);
      clearInterval(poll);
      if (error) reject(error);
      else resolve();
    };
    const check = () => { if (fs.existsSync(path)) finish(); };
    watcher = fs.watch(dirname(path), check);
    watcher.on("error", finish);
    deadline = setTimeout(() => finish(new Error(`timed out waiting for ${path}`)), 120000);
    // VirtioFS does not reliably forward host writes as guest inotify events.
    if (process.env.PI_AUTH_TEST_VIRTIOFS === "1") poll = setInterval(check, 100);
    check();
  });
}

async function worker(root, id, mode) {
  const auth = join(process.env.HOME, ".pi/agent/auth.json");
  const storage = AuthStorage.create();
  const sharedLock = `${fs.realpathSync(auth)}.lock`;
  if (mode === "lock") {
    await new FileAuthStorageBackend(auth).withLockAsync(async () => {
      assert.ok(fs.existsSync(sharedLock), "Pi used a local rather than shared lock");
      fs.writeFileSync(join(root, `locked-${id}`), "");
      await waitFor(join(root, "never-release"));
      return { result: undefined };
    });
    return;
  }
  globalThis.fetch = async (url, options) => {
    assert.equal(url, "https://auth.openai.com/oauth/token");
    assert.equal(options.method, "POST");
    assert.equal(options.body.get("grant_type"), "refresh_token");
    assert.equal(options.body.get("refresh_token"), expired.refresh);
    assert.ok(fs.existsSync(sharedLock), "refresh was not protected by the shared lock");
    fs.appendFileSync(join(root, "refreshes"), `${id}\n`);
    fs.writeFileSync(join(root, "refresh-started"), "");
    await waitFor(join(root, "finish-refresh"));
    return new Response(JSON.stringify({ access_token: jwt, refresh_token: "fixture-rotated", expires_in: 3600 }));
  };
  const models = createModels({ credentials: storage });
  models.setProvider(openaiCodexProvider());
  fs.writeFileSync(join(root, `ready-${id}`), "");
  await waitFor(join(root, "go"));
  fs.writeFileSync(join(root, `attempting-${id}`), "");
  const result = await models.getAuth("openai-codex");
  assert.equal(result.auth.apiKey, jwt);
  await storage.modify(`fixture-${id}`, () => ({ type: "api_key", key: "fixture-only" }));
  fs.writeFileSync(join(root, `done-${id}`), "");
  await waitFor(join(root, `exit-${id}`));
}

async function suite() {
  const root = fs.mkdtempSync(join(tmpdir(), "wrix-pi-auth-"));
  const children = [];
  const store = join(root, "store");
  const auth = join(store, "auth.json");
  fs.mkdirSync(store, { mode: 0o700 });
  fs.writeFileSync(auth, JSON.stringify({ "openai-codex": expired }), { mode: 0o600 });
  function start(id, mode = "refresh") {
    const home = join(root, `home-${id}`);
    fs.mkdirSync(join(home, ".pi/agent"), { recursive: true });
    fs.symlinkSync(auth, join(home, ".pi/agent/auth.json"));
    const child = spawn(process.execPath, [fileURLToPath(import.meta.url), "worker", root, id, mode], {
      env: { ...process.env, HOME: home, PI_CODING_AGENT_DIR: join(home, ".pi/agent") },
      stdio: "inherit",
    });
    const exited = once(child, "exit");
    children.push(child);
    return { child, exited };
  }
  try {
    const first = start("first");
    const second = start("second");
    await Promise.all([waitFor(join(root, "ready-first")), waitFor(join(root, "ready-second"))]);
    fs.writeFileSync(join(root, "go"), "");
    await Promise.all([waitFor(join(root, "attempting-first")), waitFor(join(root, "attempting-second")), waitFor(join(root, "refresh-started"))]);
    assert.throws(() => new FileAuthStorageBackend(join(root, "home-second/.pi/agent/auth.json")).withLock(() => ({ result: undefined })), { code: "ELOCKED" });
    fs.writeFileSync(join(root, "finish-refresh"), "");
    await Promise.all([waitFor(join(root, "done-first")), waitFor(join(root, "done-second"))]);
    assert.equal(fs.readFileSync(join(root, "refreshes"), "utf8").trim().split("\n").length, 1);
    const stored = JSON.parse(fs.readFileSync(auth, "utf8"));
    assert.equal(stored["openai-codex"].refresh, "fixture-rotated");
    assert.ok(stored["fixture-first"] && stored["fixture-second"], "concurrent provider writes were lost");
    first.child.kill("SIGKILL");
    assert.equal((await first.exited)[1], "SIGKILL");
    await AuthStorage.create(auth).delete("fixture-first");
    fs.writeFileSync(join(root, "exit-second"), "");
    assert.equal((await second.exited)[0], 0);
    assert.equal(await AuthStorage.create(auth).read("fixture-first"), undefined);
    const restart = start("restart");
    await waitFor(join(root, "done-restart"));
    fs.writeFileSync(join(root, "exit-restart"), "");
    assert.equal((await restart.exited)[0], 0);
    assert.equal(fs.readFileSync(join(root, "refreshes"), "utf8").trim().split("\n").length, 1);
    const locked = start("killed", "lock");
    await waitFor(join(root, "locked-killed"));
    // Freeze the heartbeat while advancing only the filesystem lease age.
    locked.child.kill("SIGSTOP");
    const readerAge = new Date(Date.now() - 11000);
    fs.utimesSync(`${auth}.lock`, readerAge, readerAge);
    assert.throws(() => new FileAuthStorageBackend(auth).withLock(() => ({ result: undefined })), { code: "ELOCKED" });
    locked.child.kill("SIGKILL");
    await locked.exited;
    const recovered = AuthStorage.create(auth);
    const abandonedAge = new Date(Date.now() - 31000);
    fs.utimesSync(`${auth}.lock`, abandonedAge, abandonedAge);
    await recovered.modify("fixture-recovery", () => ({ type: "api_key", key: "fixture-only" }));
    assert.equal((await recovered.read("openai-codex")).refresh, "fixture-rotated");
    assert.equal(fs.statSync(auth).mode & 0o777, 0o600);
    assert.deepEqual(fs.readdirSync(store), ["auth.json"]);
    console.log("PASS: packaged Pi shares locks, refreshes once, merges writes, and survives SIGKILL/restart");
  } finally {
    for (const child of children) {
      if (child.exitCode === null && child.signalCode === null) child.kill("SIGKILL");
    }
    fs.rmSync(root, { recursive: true, force: true });
  }
}

if (process.argv[2] === "worker") {
  await worker(process.argv[3], process.argv[4], process.argv[5]);
} else {
  await suite();
}
