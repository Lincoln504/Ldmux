// The manifest's security and lifecycle claims, checked on the source.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readFileSync } from "node:fs";
import { join } from "node:path";
import { fileURLToPath } from "node:url";

const MAIN = join(fileURLToPath(new URL("..", import.meta.url)), "composeApp", "src", "androidMain");
const manifest = readFileSync(join(MAIN, "AndroidManifest.xml"), "utf8");
const attr = (name: string): string => {
  const m = new RegExp(`${name}="([^"]*)"`).exec(manifest);
  assert.ok(m, `${name} missing from the manifest`);
  return m[1];
};

test("cleartext is governed by a network security config, not an app-wide flag", () => {
  assert.ok(!/usesCleartextTraffic/.test(manifest.replace(/<!--[\s\S]*?-->/g, "")));
  assert.equal(attr("android:networkSecurityConfig"), "@xml/network_security_config");
  const nsc = readFileSync(join(MAIN, "res", "xml", "network_security_config.xml"), "utf8");
  assert.ok(/system/.test(nsc) && !/src="user"/.test(nsc), "only system trust anchors");
});

test("the token cannot be backed up or transferred", () => {
  assert.equal(attr("android:allowBackup"), "false");
  const rules = readFileSync(join(MAIN, "res", "xml", "data_extraction_rules.xml"), "utf8");
  assert.ok(/<cloud-backup>[\s\S]*sharedpref[\s\S]*<\/cloud-backup>/.test(rules));
  assert.ok(/<device-transfer>[\s\S]*sharedpref[\s\S]*<\/device-transfer>/.test(rules));
});

test("configuration changes that can happen mid-shell do not recreate the activity", () => {
  const changes = attr("android:configChanges").split("|");
  for (const c of ["orientation", "screenSize", "fontScale", "locale", "layoutDirection", "colorMode", "navigation", "uiMode", "density"]) {
    assert.ok(changes.includes(c), `configChanges lacks ${c}`);
  }
});

test("the background claims match ADR-0012: bounded, not a promise", () => {
  assert.ok(!/is not dropped when the user switches apps/.test(manifest));
  assert.ok(/45 s/.test(manifest));
});
