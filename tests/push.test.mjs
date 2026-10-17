import assert from "node:assert/strict";
import test from "node:test";
import { loadSource } from "./load-source.mjs";

const push = loadSource("src/lib/push.ts");

const K1 = "barks://api.day.app/k1";
const K2 = "discord://hook/k2";
const F1 = "feishu://f1";

test("saved Apprise addresses keep their order, and old fields show only when the new field is empty", () => {
  assert.deepEqual(push.savedPushAddresses({ appriseUrls: `${F1};${K1}` }), [F1, K1]);
  assert.deepEqual(push.savedPushAddresses({ barkUrl: `${K1};${K2}`, feishuWebhook: F1 }), [K1, K2, F1]);
  assert.deepEqual(push.savedPushAddresses({ appriseUrls: K1, barkUrl: K2, feishuWebhook: F1 }), [K1]);
  assert.deepEqual(push.savedPushAddresses({ barkUrl: "" }), []);
});

test("rows are trimmed, split on semicolons and whitespace, and de-duplicated in order", () => {
  assert.deepEqual(push.cleanPushRows([` ${K1} `, "", `${F1};${K1}`, `${K2}\n`]), [K1, F1, K2]);
  assert.deepEqual(push.cleanPushRows(["", "  "]), []);
});

test("the same addresses in another order are not a change", () => {
  assert.equal(push.samePushUrls([K1, F1], [F1, K1]), true);
  assert.equal(push.samePushUrls([K1], [K1, F1]), false);
  assert.equal(push.samePushUrls([K1, K2], [K1, F1]), false);
});

test("the collapsed summary counts how many addresses are configured", () => {
  assert.equal(push.pushSummary([]), "尚未設定推送");
  assert.equal(push.pushSummary(push.savedPushAddresses({ appriseUrls: `${K1};${K2};${F1}` })), "推送：3 個");
  assert.equal(push.pushSummary(push.savedPushAddresses({ feishuWebhook: F1 })), "推送：1 個");
});
