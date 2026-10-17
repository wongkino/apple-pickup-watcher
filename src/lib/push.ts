import type { Settings } from "@/lib/types";

/** 文件里多个地址用分号连成一个字符串，后端保存时已去空、去重。 */
function splitStored(raw: string | undefined): string[] {
  return (raw ?? "")
    .split(";")
    .map((url) => url.trim())
    .filter((url) => url !== "");
}

/**
 * 已保存的 Apprise 地址，顺序与后端 `push_urls` 一致。
 *
 * 新配置写在 `appriseUrls`。旧文件只有 `barkUrl` / `feishuWebhook` 时先把它们
 * 显示出来，等后端规范化之后才会改写成 Apprise 地址并清空旧栏。
 */
export function savedPushAddresses(settings: Settings): string[] {
  const apprise = splitStored(settings.appriseUrls);
  if (apprise.length > 0) return apprise;
  return [...splitStored(settings.barkUrl), ...splitStored(settings.feishuWebhook)];
}

/**
 * 把编辑中的几行整理成要保存的地址：去掉首尾空白和空行；一行里粘了好几个地址
 * （老版本用分号连着的写法）就拆开；重复的只留第一个。
 */
export function cleanPushRows(rows: string[]): string[] {
  const out: string[] = [];
  for (const row of rows) {
    for (const piece of row.split(/[;\s]+/)) {
      const url = piece.trim();
      if (url !== "" && !out.includes(url)) out.push(url);
    }
  }
  return out;
}

/** 两组地址是否相同，不计顺序：只是顺序变了，不必重写设置文件。 */
export function samePushUrls(a: string[], b: string[]): boolean {
  return a.length === b.length && a.every((url) => b.includes(url));
}

/** 设置收起时摘要里的那一段，例如「推送：2 个」。 */
export function pushSummary(addresses: string[]): string {
  if (addresses.length === 0) return "尚未設定推送";
  return `推送：${addresses.length} 個`;
}
