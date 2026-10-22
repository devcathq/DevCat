// Optional extras; the page works without them. Download links point at the files of the latest
// release (their names carry the version), the main button at this computer's installer, and the
// install commands get a Copy button.
const RELEASES = "https://api.github.com/repos/devcathq/DevCat/releases/latest";

function platform() {
  const ua = navigator.userAgent;
  if (/Android/i.test(ua)) return { name: "Android", asset: "DevCat-Mobile.apk" };
  if (/iPhone|iPad/i.test(ua)) return null;
  if (/Windows/i.test(ua)) return { name: "Windows", asset: "_x64-setup.exe" };
  // Browsers report Intel on every Mac; Apple silicon is far more common now.
  if (/Mac/i.test(ua)) return { name: "macOS", asset: "_aarch64.dmg" };
  if (/Linux/i.test(ua)) return { name: "Linux", asset: "_amd64.AppImage" };
  return null;
}

async function wireDownloads() {
  const res = await fetch(RELEASES, { headers: { Accept: "application/vnd.github+json" } });
  if (!res.ok) return;
  const rel = await res.json();
  const find = (suffix) => rel.assets.find((a) => a.name.endsWith(suffix));
  for (const a of document.querySelectorAll(".dl[data-asset]")) {
    const asset = find(a.dataset.asset);
    if (asset) a.href = asset.browser_download_url;
  }
  const me = platform();
  const asset = me && find(me.asset);
  if (!asset) return;
  const main = document.getElementById("dl-main");
  main.href = asset.browser_download_url;
  main.textContent = `Download for ${me.name}`;
  document.getElementById("dl-note").textContent = `DevCat ${rel.tag_name.replace(/^v/, "")}, ${(asset.size / 1048576).toFixed(0)} MB. No account needed.`;
  document.querySelector(`.dl[data-asset="${me.asset}"]`)?.classList.add("mine");
}

function addCopyButtons() {
  for (const pre of document.querySelectorAll(".cmd pre")) {
    const b = document.createElement("button");
    b.className = "copy";
    b.type = "button";
    b.textContent = "Copy";
    b.addEventListener("click", async () => {
      try {
        await navigator.clipboard.writeText(pre.querySelector("code").textContent.trim());
        b.textContent = "Copied";
      } catch {
        b.textContent = "Select and copy";
      }
      setTimeout(() => (b.textContent = "Copy"), 1600);
    });
    pre.append(b);
  }
}

addCopyButtons();
wireDownloads().catch(() => {});
