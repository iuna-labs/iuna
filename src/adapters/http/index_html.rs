pub(super) const INDEX_HTML: &str = concat!(
    r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="/favicon.ico" sizes="any">
  <title>iuna</title>
  <style>
    [x-cloak] { display: none !important; }
    .visually-hidden { position: absolute !important; width: 1px !important; height: 1px !important; overflow: hidden !important; clip: rect(0 0 0 0) !important; clip-path: inset(50%) !important; white-space: nowrap !important; }
    :root {
      color-scheme: dark;
      font-family: Inter, ui-sans-serif, system-ui, -apple-system, BlinkMacSystemFont, "Segoe UI", sans-serif;
      background: #0f1012;
      color: #e8edf0;
    }
    * { box-sizing: border-box; }
    html { width: 100%; max-width: 100%; overflow-x: hidden; }
    body { margin: 0; min-height: 100vh; max-width: 100%; overflow-x: hidden; background: #0f1012; color: #e8edf0; }
    .app-shell { width: 100%; max-width: 100%; min-height: 100vh; display: block; overflow-x: hidden; }
    .sidebar { position: fixed; z-index: 5; inset: 0 auto 0 0; width: 84px; height: 100vh; display: flex; flex-direction: column; align-items: center; gap: 20px; padding: 16px 10px; background: #15171a; border-right: 1px solid #262b2f; }
    .brand-mark { position: relative; width: 64px; min-height: 58px; display: grid; place-items: center; overflow: hidden; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; background: transparent; color: #d5f55f; user-select: none; cursor: pointer; }
    .brand-mark::after { content: ""; position: absolute; inset: -40% -70%; background: linear-gradient(100deg, transparent 42%, rgba(255, 255, 255, .34) 50%, transparent 58%); transform: translateX(-58%) rotate(8deg); opacity: 0; pointer-events: none; }
    .brand-mark svg { position: relative; z-index: 1; width: 24px; height: 24px; display: block; }
    .brand-mark .mark-loop { fill: none; stroke: currentColor; stroke-width: 4.2; stroke-linecap: round; stroke-linejoin: round; }
    .brand-mark .mark-dot { fill: currentColor; }
    .brand-mark:hover, .brand-mark.active { border-color: #3b4448; background: #202328; }
    .brand-mark:hover::after { animation: mark-sheen .72s ease both; }
    @keyframes mark-sheen { from { opacity: 0; transform: translateX(-58%) rotate(8deg); } 32% { opacity: 1; } to { opacity: 0; transform: translateX(58%) rotate(8deg); } }
    .side-nav { display: grid; gap: 10px; width: 100%; }
    .nav-button { width: 64px; min-height: 58px; display: grid; place-items: center; gap: 4px; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; background: transparent; color: #9fa8ad; }
    .nav-button svg { width: 21px; height: 21px; stroke: currentColor; stroke-width: 2; fill: none; }
    .nav-button svg.chain-icon { stroke-width: 1.35; }
    .nav-button span { font-size: 11px; font-weight: 800; }
    .nav-button:hover, .nav-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .sidebar-bottom-actions { width: 100%; display: grid; gap: 6px; margin-top: auto; }
    .discord-button { width: 64px; height: 34px; min-height: 34px; display: grid; place-items: center; gap: 2px; border: 1px solid rgba(88, 101, 242, .72); border-radius: 8px; padding: 5px 4px; background: rgba(88, 101, 242, .16); color: #eef1ff; text-align: center; text-decoration: none; }
    .discord-button svg { width: 18px; height: 18px; fill: currentColor; }
    .discord-button span { display: none; font-size: 10px; font-weight: 800; }
    .discord-button:hover { border-color: #8e99ff; background: rgba(88, 101, 242, .28); color: #fff; }
    .settings-button { width: 64px; min-height: 54px; display: grid; place-items: center; gap: 4px; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #9fa8ad; background: transparent; text-align: center; }
    .settings-button svg { width: 23px; height: 23px; stroke: currentColor; stroke-width: 1.9; fill: none; }
    .settings-button span { display: none; font-size: 10px; font-weight: 800; }
    .settings-button:hover, .settings-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .brand-mark, .nav-button, .discord-button, .settings-button { transition: filter .1s ease, background-color .14s ease, border-color .14s ease, color .14s ease; }
    .brand-mark:active, .nav-button:active, .discord-button:active, .settings-button:active { filter: brightness(1.14); }
    .version-panel { width: 64px; display: grid; gap: 4px; justify-items: center; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #7f888e; background: transparent; font-size: 10px; font-weight: 850; text-align: center; }
    .version-panel.update { border-color: #566d25; color: #d5f55f; background: #1c2516; cursor: pointer; }
    .version-panel.checking { color: #a8b2b8; }
    .version-panel.failed { color: #ffb1a8; }
    .version-dot { width: 6px; height: 6px; border-radius: 999px; background: #3a4248; }
    .version-panel.update .version-dot { background: #d5f55f; box-shadow: 0 0 0 3px rgba(213, 245, 95, .12); }
    .version-panel.failed .version-dot { background: #ff8f82; }
    .version-label { line-height: 1; }
    .version-update { color: #d5f55f; font-size: 9px; line-height: 1; text-transform: uppercase; }
    .content { width: 100%; min-width: 0; min-height: 100vh; min-height: 100dvh; display: flex; flex-direction: column; overflow-x: hidden; padding: 22px 24px 48px 108px; }
    main { width: 100%; }
    main > section { width: 100%; }
    header { display: flex; justify-content: space-between; gap: 18px; align-items: flex-start; padding: 0 0 18px; }
    .header-actions { display: flex; gap: 10px; align-items: center; }
    .basic-status-row { display: inline-flex; gap: 8px; align-items: center; margin-top: 5px; }
    .basic-status { display: inline-flex; gap: 7px; align-items: center; border: 1px solid transparent; border-radius: 999px; padding: 3px 7px; color: #9eb3bc; font-size: 12px; font-weight: 800; }
    .basic-status::before { content: ""; width: 6px; height: 6px; flex: 0 0 auto; border-radius: 999px; background: #7f888e; }
    .basic-status.healthy { color: #d5f55f; }
    .basic-status.healthy::before { background: #d5f55f; box-shadow: 0 0 12px rgba(213, 245, 95, .45); }
    .basic-status.syncing { color: #ffd070; }
    .basic-status.syncing::before { background: #ffd070; }
    .basic-status.isolated, .basic-status.stale, .basic-status.banned, .basic-status.error { border-color: #6a332c; background: #261817; color: #ffb8ad; font-weight: 900; }
    .basic-status.isolated::before, .basic-status.stale::before, .basic-status.banned::before, .basic-status.error::before { background: #ff7668; box-shadow: 0 0 12px rgba(255, 118, 104, .38); }
    .basic-status-detail { padding: 3px 7px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 11px; }
    .basic-status-detail:hover { border-color: #d5f55f; color: #d5f55f; }
    .lock-button { padding: 5px 8px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 12px; }
    .lock-button:hover { border-color: #d5f55f; color: #d5f55f; }
    h1 { margin: 0 0 4px; font-size: 28px; }
    h2 { margin: 0 0 12px; font-size: 18px; }
    h3 { margin: 0 0 10px; font-size: 15px; }
    button { border: 1px solid #3a4248; border-radius: 6px; padding: 8px 11px; font: inherit; font-weight: 700; background: #191c20; color: #e8edf0; cursor: pointer; }
    button:hover { border-color: #d5f55f; color: #d5f55f; }
    button.primary { background: #d5f55f; border-color: #d5f55f; color: #15171a; }
    button.primary:hover { background: #e4ff83; color: #15171a; }
    button.subtle { background: transparent; }
    button.danger { border-color: #8f3730; background: #341918; color: #ffb1a8; }
    button.danger:hover { border-color: #ff7668; background: #451c1a; color: #ffd8d3; }
    button:disabled { cursor: default; opacity: .5; }
    .grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; }
    .metric, .panel { background: #181b1f; border: 1px solid #2a3035; border-radius: 8px; padding: 13px; }
    .metric .label { color: #8d989f; font-size: 11px; text-transform: uppercase; }
    .metric .value { margin-top: 7px; font-weight: 850; overflow-wrap: anywhere; }
    .panel { margin-bottom: 12px; }
    .dashboard-section { min-height: 0; flex: 1; display: grid; place-items: center; }
    .dashboard-grid { width: 100%; display: grid; grid-template-columns: repeat(5, minmax(0, 156px)); justify-content: center; align-content: center; gap: 10px; }
    .dashboard-card { aspect-ratio: 1; min-width: 0; display: flex; flex-direction: column; justify-content: center; align-items: center; gap: 13px; border: 1px solid #2a3035; border-radius: 9px; padding: 15px; background: #181b1f; text-align: center; }
    .dashboard-card-icon { width: 52px; height: 52px; display: grid; place-items: center; flex: 0 0 auto; border-radius: 10px; background: #111316; color: #9fa8ad; }
    .dashboard-card-icon svg { width: 31px; height: 31px; fill: none; stroke: currentColor; stroke-width: 1.6; stroke-linecap: round; stroke-linejoin: round; }
    .dashboard-card.good .dashboard-card-icon { background: #1c2516; color: #d5f55f; }
    .dashboard-card.warning .dashboard-card-icon { background: #292315; color: #ffd070; }
    .dashboard-card.bad .dashboard-card-icon { background: #2a1717; color: #ff8f82; }
    .dashboard-card-copy { min-width: 0; width: 100%; display: grid; justify-items: center; }
    .dashboard-card-label { margin-bottom: 6px; color: #8d989f; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .dashboard-card-value { color: #d5f55f; font-size: 14px; font-weight: 850; font-variant-numeric: tabular-nums; line-height: 1.25; overflow-wrap: anywhere; }
    .dashboard-card.balance { padding-inline: 10px; }
    .dashboard-card.balance .dashboard-card-value, .dashboard-card.burning .dashboard-status { font-size: clamp(10px, 1.25vw, 13px); letter-spacing: -.015em; white-space: nowrap; }
    .dashboard-card.burning .dashboard-status { width: 100%; text-align: center; }
    .dashboard-status { color: #8d989f; font-size: 14px; font-weight: 850; font-variant-numeric: tabular-nums; line-height: 1.25; overflow-wrap: anywhere; }
    .dashboard-card.good .dashboard-status { color: #d5f55f; }
    .dashboard-card.warning .dashboard-status { color: #ffd070; }
    .dashboard-card.bad .dashboard-status { color: #ff9d91; }
    .split { display: grid; grid-template-columns: minmax(0, 1fr) minmax(320px, .72fr); gap: 12px; }
    form { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    label { display: grid; gap: 5px; color: #a8b2b8; font-size: 13px; }
    input, textarea { min-width: 180px; border: 1px solid #3a444b; border-radius: 6px; padding: 9px 10px; font: inherit; background: #101215; color: #edf2f5; }
    textarea { min-height: 118px; resize: vertical; line-height: 1.45; }
    input:focus, textarea:focus { outline: 2px solid #d5f55f; outline-offset: 1px; }
    table { width: 100%; border-collapse: collapse; font-size: 13px; }
    th, td { text-align: left; border-bottom: 1px solid #2a3035; padding: 8px; vertical-align: top; }
    th { color: #8d989f; font-size: 11px; text-transform: uppercase; }
    code { overflow-wrap: anywhere; color: #c7f5ea; }
    .table-wrap { max-width: 100%; overflow-x: auto; overscroll-behavior-x: contain; -webkit-overflow-scrolling: touch; }
    .muted { color: #8d989f; }
    .flash { position: fixed; top: 18px; right: 18px; z-index: 80; width: min(420px, calc(100vw - 36px)); border-radius: 6px; padding: 10px 12px; border: 1px solid; font-weight: 700; box-shadow: 0 18px 48px rgba(0, 0, 0, .38); }
    .flash.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .flash.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .persistent-banner { border: 1px solid #566d25; border-radius: 8px; padding: 10px 12px; margin: -4px 0 16px; color: #d5f55f; background: #1c2516; font-weight: 800; }
    .ok { color: #d5f55f; }
    .page-title { margin-bottom: 16px; }
    .setup-overlay { position: fixed; inset: 0; z-index: 30; display: grid; place-items: center; padding: 22px; background: rgba(8, 9, 10, .72); backdrop-filter: blur(8px); }
    .transaction-overlay { z-index: 40; }
    .sync-overlay { position: fixed; inset: 0; z-index: 60; display: grid; place-items: center; padding: 22px; background: #0f1012; }
    .sync-screen { width: min(460px, 100%); display: grid; justify-items: center; gap: 18px; text-align: center; }
    .sync-mark { width: 54px; height: 54px; display: grid; place-items: center; border: 1px solid #566d25; border-radius: 14px; background: #1c2516; box-shadow: 0 18px 48px rgba(0, 0, 0, .32); }
    .sync-spinner { width: 25px; height: 25px; border: 3px solid rgba(213, 245, 95, .2); border-top-color: #d5f55f; border-radius: 999px; animation: sync-spin .9s linear infinite; }
    .sync-screen h1 { margin: 0; font-size: clamp(25px, 5vw, 36px); }
    .sync-copy { margin: -8px 0 0; color: #9fa8ad; line-height: 1.5; }
    .sync-progress { width: 100%; height: 9px; overflow: hidden; border: 1px solid #30373b; border-radius: 999px; background: #191c1f; }
    .sync-progress-fill { height: 100%; border-radius: inherit; background: linear-gradient(90deg, #8de9cd, #d5f55f); transition: width .35s ease; }
    .sync-progress-label { color: #d5f55f; font-size: 14px; font-weight: 850; font-variant-numeric: tabular-nums; }
    @keyframes sync-spin { to { transform: rotate(360deg); } }
    @media (prefers-reduced-motion: reduce) { .sync-spinner { animation-duration: 1.8s; } .sync-progress-fill, .brand-mark, .nav-button, .discord-button, .settings-button { transition: none; } .brand-mark:hover::after { animation: none; } }
    .setup-modal { width: min(980px, 100%); max-height: calc(100vh - 44px); overflow: auto; border: 1px solid #3b4448; border-radius: 8px; padding: 18px; background: #181b1f; box-shadow: 0 24px 80px rgba(0, 0, 0, .42); }
    .setup-modal-head { display: grid; gap: 5px; margin-bottom: 16px; }
    .setup-modal-head h2 { margin: 0; font-size: 24px; }
    .setup-welcome { color: #d5f55f; font-size: 12px; font-weight: 900; text-transform: uppercase; }
    .setup-copy { max-width: 620px; color: #a8b2b8; line-height: 1.45; }
    .setup-feedback { border: 1px solid; border-radius: 8px; padding: 10px 12px; margin-bottom: 14px; font-weight: 800; }
    .setup-feedback.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .setup-feedback.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .setup-grid { width: 100%; display: grid; grid-template-columns: minmax(0, .9fr) minmax(320px, .7fr); gap: 12px; align-items: start; }
    .setup-section { border: 1px solid #2f363c; border-radius: 8px; padding: 13px; background: #111316; }
    .setup-node-mode, .setup-network, .setup-wallet-section { grid-column: 1 / -1; }
    .segmented.setup-mode-picker { width: 100%; display: grid; grid-template-columns: repeat(3, minmax(0, 1fr)); }
    .setup-mode-picker button { min-width: 0; min-height: 38px; white-space: normal; }
    .setup-network-row { display: grid; grid-template-columns: minmax(0, 1fr); gap: 10px; align-items: end; }
    .setup-network-copy { margin-top: 8px; color: #a8b2b8; line-height: 1.45; }
    .setup-network-link { color: #d5f55f; font-size: 12px; font-weight: 900; text-decoration: none; }
    .setup-network-link:hover { text-decoration: underline; }
    .setup-field { display: grid; gap: 6px; }
    .setup-field-label { color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; letter-spacing: 0; }
    .setup-address-box { display: flex; justify-content: space-between; gap: 10px; align-items: center; }
    .setup-address-box code { min-width: 0; }
    .setup-address-box button { flex: 0 0 auto; }
    .setup-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 14px; }
    .segmented { display: inline-flex; gap: 4px; padding: 4px; border: 1px solid #2f363c; border-radius: 8px; background: #181b1f; }
    .segmented button { border-color: transparent; background: transparent; color: #9fa8ad; }
    .segmented button.active { background: #d5f55f; color: #15171a; }
    .seed-panel { display: grid; gap: 12px; }
    .seed-grid { display: grid; grid-template-columns: repeat(4, minmax(0, 1fr)); gap: 8px; }
    .seed-word { display: grid; grid-template-columns: 30px minmax(0, 1fr); gap: 7px; align-items: center; border: 1px solid #2f363c; border-radius: 6px; padding: 7px 8px; background: #181b1f; }
    .seed-word .index { color: #8d989f; font-size: 11px; font-weight: 800; }
    .seed-word .word { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-weight: 800; color: #c7f5ea; }
    .verify-grid { display: grid; gap: 8px; }
    .setup-status { border: 1px solid #566d25; border-radius: 8px; padding: 10px; background: #1c2516; color: #d5f55f; font-weight: 800; }
    .auth-form { width: min(420px, 100%); display: grid; gap: 10px; }
    .auth-form form { display: grid; gap: 10px; align-items: stretch; }
    .auth-form input { width: 100%; }
    .settings-grid { width: min(760px, 100%); display: grid; gap: 12px; }
    .settings-mode-row { display: flex; justify-content: space-between; gap: 14px; align-items: center; }
    .settings-mode-copy { min-width: 0; display: grid; gap: 4px; }
    .settings-mode-title { color: #e8edf0; font-size: 15px; font-weight: 850; }
    .settings-form { display: grid; gap: 10px; align-items: stretch; }
    .public-p2p-form { margin-top: 14px; }
    .settings-form label, .settings-form input { width: 100%; }
    .danger-panel { border-color: #6a332c; background: #201313; }
    .danger-panel h3, .danger-title { color: #ffb1a8; }
    .danger-copy { color: #d69a92; line-height: 1.45; }
    .danger-actions { display: flex; justify-content: flex-end; gap: 10px; margin-top: 12px; }
    .metrics-shell { display: grid; gap: 12px; }
    .metrics-head { display: flex; justify-content: flex-end; gap: 12px; align-items: center; }
    .metrics-range { flex: 0 0 auto; }
    .metrics-range button { padding: 5px 9px; font-size: 12px; white-space: nowrap; }
    .metrics-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 10px; }
    .metrics-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 430px), 1fr)); gap: 12px; }
    .metric-chart-card { display: grid; gap: 10px; min-width: 0; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; background: #181b1f; }
    .metric-chart-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; }
    .metric-chart-title { margin: 0; color: #e8edf0; font-size: 14px; font-weight: 850; }
    .metric-chart-value { color: #d5f55f; font-size: 13px; font-weight: 850; font-variant-numeric: tabular-nums; }
    .metric-chart-frame { display: grid; grid-template-columns: 50px minmax(0, 1fr); grid-template-rows: 156px 18px; column-gap: 6px; row-gap: 4px; min-width: 0; }
    .metric-chart-plot { position: relative; min-width: 0; }
    .metric-chart-svg { width: 100%; height: 156px; display: block; border: 1px solid #2f363c; border-radius: 8px; background: #111316; }
    .metric-chart-gridline { stroke: #3a4248; stroke-width: .8; stroke-dasharray: 3 7; opacity: .58; }
    .metric-chart-axis { stroke: #3a4248; stroke-width: 1.2; }
    .metric-chart-axis-label { color: #8d989f; font-size: 10px; font-weight: 750; font-variant-numeric: tabular-nums; overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
    .metric-chart-y-axis { position: relative; min-width: 0; }
    .metric-chart-y-axis .metric-chart-axis-label { position: absolute; right: 0; transform: translateY(-50%); max-width: 100%; }
    .metric-chart-x-axis { position: relative; grid-column: 2; min-width: 0; overflow: visible; }
    .metric-chart-x-axis .metric-chart-axis-label { position: absolute; top: 0; transform: translateX(-50%); }
    .metric-chart-line { fill: none; stroke: #d5f55f; stroke-width: 2.2; stroke-linejoin: round; stroke-linecap: round; }
    .metric-chart-hover-point { position: absolute; width: 8px; height: 8px; border-radius: 50%; background: #d5f55f; pointer-events: none; transform: translate(-50%, -50%); box-shadow: 0 0 0 4px rgba(213, 245, 95, .18); }
    .metric-chart-tooltip { position: absolute; z-index: 1; max-width: min(180px, 80%); border: 1px solid #566d25; border-radius: 6px; padding: 5px 7px; background: #202615; color: #e8edf0; font-size: 11px; font-weight: 850; font-variant-numeric: tabular-nums; line-height: 1.25; pointer-events: none; box-shadow: 0 8px 20px rgba(0, 0, 0, .28); white-space: nowrap; }
    .metrics-empty { border: 1px dashed #3a4248; border-radius: 8px; padding: 14px; color: #8d989f; background: #111316; }
    .leaderboard-tabs { width: 100%; display: flex; gap: 12px; margin-bottom: 16px; border-bottom: 1px solid #2f363c; overflow-x: auto; scrollbar-width: none; }
    .leaderboard-tabs::-webkit-scrollbar { display: none; }
    .leaderboard-tabs button { flex: 1 0 auto; border: 0; border-bottom: 2px solid transparent; border-radius: 0; padding: 6px 2px 8px; background: transparent; color: #8d989f; font-size: 12px; white-space: nowrap; }
    .leaderboard-tabs button:hover { border-bottom-color: #566d25; color: #d6dee2; }
    .leaderboard-tabs button.active { border-bottom-color: #d5f55f; background: transparent; color: #d5f55f; }
    .leaderboard-grid { width: min(100%, 400px); justify-self: center; display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; margin-top: 8px; }
    .leaderboard-card { min-width: 0; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; background: #181b1f; }
    .leaderboard-card h3 { margin: 0 0 10px; }
    .leaderboard-list { display: grid; gap: 8px; }
    .leaderboard-row { display: grid; grid-template-columns: 54px minmax(0, 1fr) auto; gap: 10px; align-items: center; border: 1px solid #2f363c; border-radius: 8px; padding: 9px; background: #111316; }
    .leaderboard-row-empty { opacity: .45; }
    .leaderboard-rank { display: grid; place-items: center; min-height: 30px; border: 1px solid #3a4248; border-radius: 999px; color: #a8b2b8; font-size: 11px; font-weight: 900; }
    .leaderboard-rank.medal-1 { border-color: #ffd070; background: #2d2513; color: #ffd070; }
    .leaderboard-rank.medal-2 { border-color: #c7d0d5; background: #20252a; color: #e8edf0; }
    .leaderboard-rank.medal-3 { border-color: #c69262; background: #2a1f17; color: #ffc18a; }
    .leaderboard-main { min-width: 0; display: grid; gap: 3px; }
    .leaderboard-amount { color: #d5f55f; font-size: 13px; font-weight: 900; font-variant-numeric: tabular-nums; white-space: nowrap; }
    .wallet-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .wallet-actions { display: grid; gap: 12px; }
    .optimize-form { display: grid; gap: 12px; }
    .optimize-form .optimize-choice { display: flex; align-items: center; gap: 8px; }
    .optimize-choice input { width: auto; min-width: auto; margin: 0; }
    .optimize-form button { justify-self: start; }
    .amount-field { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; align-items: end; }
    .amount-field label { min-width: 0; }
    .amount-field input { width: 100%; }
    .amount-max-button { padding: 9px 10px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 12px; text-transform: uppercase; }
    .amount-max-button:hover { border-color: #d5f55f; color: #d5f55f; }
    .advanced-toggle { flex-basis: 100%; width: max-content; align-self: flex-start; border-color: #3a4248; padding: 4px 7px; background: #202328; color: #9fa8ad; font-size: 12px; }
    .advanced-toggle:hover { border-color: #5a646b; color: #d6dee2; }
    .send-utxo-list { display: grid; gap: 8px; max-height: 260px; overflow: auto; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #111316; }
    .send-utxo-list-head { display: flex; justify-content: space-between; gap: 8px; align-items: center; color: #8d989f; font-size: 12px; font-weight: 800; }
    .send-utxo-actions { display: flex; gap: 6px; align-items: center; }
    .utxo-select-button { padding: 3px 7px; border-color: #3a4248; background: #202328; color: #9fa8ad; font-size: 12px; }
    .utxo-select-button:hover { border-color: #5a646b; color: #d6dee2; }
    .send-utxo-option { display: grid; grid-template-columns: auto minmax(0, 1fr); gap: 8px; align-items: start; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #181b1f; user-select: none; }
    .send-utxo-option.disabled { border-color: #262c31; background: #14171a; color: #687178; }
    .send-utxo-option.disabled code, .send-utxo-option.disabled .utxo-node-amount { color: #687178; }
    .send-utxo-option input { min-width: auto; margin-top: 3px; pointer-events: none; }
    .utxo-status { color: #8d989f; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .send-utxo-summary { flex-basis: 100%; width: 100%; display: grid; gap: 5px; color: #9eb3bc; font-size: 13px; }
    .wallet-balance-line { display: inline-grid; grid-template-columns: auto auto; gap: 10px; align-items: baseline; padding: 8px 10px; border: 1px solid #2f363c; border-radius: 8px; background: #111316; color: inherit; cursor: pointer; }
    .wallet-balance-line:hover, .wallet-balance-line:focus-visible { border-color: #d5f55f; outline: none; }
    .wallet-balance-line .tx-value { font-size: 16px; font-weight: 850; }
    .mining-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr); gap: 12px; align-items: start; }
    .panel-description { max-width: 760px; margin: -4px 0 12px; color: #9eb3bc; font-size: 13px; line-height: 1.45; }
    .mining-form { width: 100%; display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .burn-fields { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .mine-action-row { display: grid; grid-template-columns: minmax(0, 1fr) auto auto; gap: 12px; align-items: center; }
    .mine-settings-form { display: grid; gap: 10px; }
    .mine-fee-fields { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .fee-preview { flex-basis: 100%; color: #9eb3bc; font-size: 12px; font-weight: 700; }
    .fee-preview.error { color: #ffb1a8; }
    .fee-warning { flex-basis: 100%; color: #ffd070; font-size: 12px; font-weight: 800; }
    .mine-stats { display: grid; grid-template-columns: repeat(4, minmax(112px, 1fr)); gap: 8px; min-width: 0; }
    .local-mining-stats { grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); }
    .fee-history { grid-template-columns: repeat(3, minmax(112px, 1fr)); margin-top: 12px; }
    .mine-stat { min-width: 0; border: 1px solid #2f363c; border-radius: 8px; padding: 9px 10px; background: #111316; }
    .mine-stat-label { display: flex; gap: 5px; align-items: center; color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .mine-stat-value { margin-top: 5px; color: #dce4e7; font-size: 14px; font-weight: 850; font-variant-numeric: tabular-nums; overflow-wrap: anywhere; }
    .mine-stat-value.money { color: #d5f55f; }
    .mine-reward-control { display: grid; gap: 7px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .mine-reward-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; color: #a8b2b8; font-size: 13px; font-weight: 800; }
    .mine-reward-head strong { color: #d5f55f; font-size: 14px; font-variant-numeric: tabular-nums; }
    .mine-reward-control input[type="range"] { width: 100%; min-width: 0; padding: 0; accent-color: #d5f55f; }
    .mine-slider-hints { display: flex; justify-content: space-between; gap: 10px; color: #879198; font-size: 11px; font-weight: 750; }
    .mine-include-status { display: flex; flex-wrap: wrap; justify-content: space-between; gap: 7px; color: #9eb3bc; font-size: 12px; font-weight: 800; }
    .mine-include-status.waiting { color: #ffd280; }
    .mine-include-status.ready { color: #d5f55f; }
    .mine-include-status.muted { color: #879198; }
    .mine-save-row { display: flex; justify-content: flex-start; }
    .mining-event-log { display: grid; gap: 8px; max-height: 360px; overflow: auto; margin-top: 12px; border: 1px solid #2f363c; border-radius: 8px; padding: 8px; background: #0f1114; }
    .mining-event-log-head { display: flex; justify-content: space-between; gap: 10px; align-items: baseline; color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .mining-event { display: grid; grid-template-columns: auto minmax(0, 1fr) auto; gap: 10px; align-items: start; border: 1px solid #30383d; border-radius: 8px; padding: 10px; background: #111316; }
    .mining-event-dot { width: 8px; height: 8px; margin-top: 5px; border-radius: 999px; background: #7f888e; }
    .mining-event.active .mining-event-dot { background: #d5f55f; box-shadow: 0 0 12px rgba(213, 245, 95, .45); }
    .mining-event.warning .mining-event-dot { background: #ffd070; }
    .mining-event.info .mining-event-dot { background: #8de9cd; }
    .mining-event-title { color: #eef6f8; font-weight: 850; overflow-wrap: anywhere; }
    .mining-event-detail { margin-top: 3px; color: #9fa8ad; font-size: 12px; line-height: 1.35; overflow-wrap: anywhere; }
    .mining-event-time { color: #7f888e; font-size: 11px; font-weight: 800; white-space: nowrap; }
    .mining-event-empty { height: 42px; border: 1px solid #30383d; border-radius: 8px; background: #111316; }
    .mining-event-empty .skeleton-line { width: 100%; height: 100%; border-radius: 8px; opacity: .55; }
    .panel-separator { border-top: 1px solid #2f363c; margin: 14px 0 12px; }
    .stratum-config { display: grid; gap: 10px; }
    .stratum-note { max-width: 760px; color: #9eb3bc; font-size: 12px; line-height: 1.45; }
    .stratum-note code { color: #dce4e7; }
    .stratum-fields { display: grid; gap: 8px; }
    .stratum-field { display: grid; grid-template-columns: 86px minmax(0, 1fr); gap: 10px; align-items: baseline; min-width: 0; }
    .stratum-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .stratum-value { min-width: 0; color: #dce4e7; font-size: 13px; font-weight: 650; overflow-wrap: anywhere; }
    .stratum-value.hash { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; font-weight: 500; }
    .info-button { display: inline-grid; place-items: center; width: 18px; height: 18px; padding: 0; border-radius: 999px; border-color: #3a4248; background: #181b1f; color: #9eb3bc; font-size: 11px; line-height: 1; }
    .info-button:hover, .info-button:focus-visible { border-color: #d5f55f; color: #d5f55f; outline: none; }
    .info-copy { display: grid; gap: 10px; color: #c3cbd0; line-height: 1.45; }
    .info-copy p { margin: 0; }
    .info-facts { display: grid; grid-template-columns: repeat(auto-fit, minmax(150px, 1fr)); gap: 8px; }
    .info-fact { border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .info-fact .label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .info-fact .value { margin-top: 5px; color: #d5f55f; font-weight: 850; }
    .mining-head { align-items: center; }
    .toggle-switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 9px; color: #9fa8ad; font-size: 12px; font-weight: 850; cursor: pointer; user-select: none; }
    .toggle-switch input { position: absolute; width: 1px; height: 1px; min-width: 0; margin: 0; opacity: 0; pointer-events: none; }
    .toggle-track { position: relative; width: 46px; height: 26px; border: 1px solid #3a4248; border-radius: 999px; background: #101215; transition: background .16s ease, border-color .16s ease; }
    .toggle-thumb { position: absolute; top: 3px; left: 3px; width: 18px; height: 18px; border-radius: 999px; background: #879198; transition: transform .16s ease, background .16s ease; }
    .toggle-switch.active { color: #d5f55f; }
    .toggle-switch.active .toggle-track { border-color: #d5f55f; background: #263219; }
    .toggle-switch.active .toggle-thumb { transform: translateX(20px); background: #d5f55f; }
    .toggle-switch:focus-within .toggle-track { outline: 2px solid #d5f55f; outline-offset: 2px; }
    .toggle-text { min-width: 22px; text-align: right; }
    .compact-number-field { display: inline-flex; align-items: center; gap: 8px; color: #9fa8ad; font-size: 12px; font-weight: 850; }
    .compact-number-field input { width: 58px; min-width: 0; border: 1px solid #3a4248; border-radius: 8px; padding: 7px 8px; background: #101215; color: #dce4e7; font: inherit; font-variant-numeric: tabular-nums; }
    .compact-number-field input:focus { border-color: #d5f55f; outline: 2px solid rgba(213,245,95,.2); outline-offset: 2px; }
    .receive-address { display: grid; gap: 8px; }
    .address-box { border: 1px solid #2f363c; border-radius: 8px; padding: 11px; background: #111316; }
    .wallet-address-summary { display: flex; justify-content: space-between; gap: 12px; align-items: center; width: 100%; padding: 9px 10px; text-align: left; color: #b9c2c7; background: #111316; border-color: #2f363c; }
    .wallet-address-summary:hover, .wallet-address-summary:focus-visible { border-color: #59656c; color: #eef6f8; outline: none; }
    .wallet-address-summary-action { flex: 0 0 auto; color: #d5f55f; font-weight: 800; }
    .wallet-address-list { display: grid; gap: 8px; }
    .wallet-address-row { display: grid; gap: 8px; border: 1px solid #2f363c; border-radius: 8px; padding: 11px; background: #111316; }
    .wallet-address-row-head { display: flex; justify-content: space-between; gap: 10px; align-items: center; }
    .wallet-address-state { color: #9eb3bc; font-size: 11px; font-weight: 850; text-transform: uppercase; }
    .wallet-address-state.pending { color: #efca75; }
    .wallet-address-link { cursor: pointer; text-decoration: underline; text-decoration-style: dotted; text-decoration-color: #59656c; text-underline-offset: 3px; }
    .wallet-address-link:hover, .wallet-address-link:focus-visible { color: #d5f55f; text-decoration-color: #d5f55f; outline: none; }
    .address-book-list { display: grid; gap: 8px; margin-top: 12px; }
    .address-book-row { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; align-items: center; text-align: left; border: 1px solid #2f363c; border-radius: 8px; padding: 9px; background: #111316; color: inherit; }
    .address-book-row:hover { border-color: #4c565c; background: #15181b; }
    .address-book-row > svg { width: 18px; height: 18px; stroke: currentColor; stroke-width: 2; fill: none; stroke-linecap: round; stroke-linejoin: round; color: #9fa8ad; }
    .address-book-name { font-weight: 800; color: #eef6f8; overflow-wrap: anywhere; }
    .address-book-actions { display: flex; gap: 6px; align-items: center; }
    .address-book-modal { width: min(560px, 100%); }
    .address-book-modal form { width: 100%; display: grid; gap: 10px; }
    .address-book-form { display: grid; gap: 10px; margin-top: 14px; padding-top: 14px; border-top: 1px solid #2f363c; }
    .address-book-modal-actions { flex: 1 0 100%; width: 100%; display: flex; justify-content: flex-end; gap: 8px; margin-top: 12px; }
    .address-book-picker-list { display: grid; gap: 8px; }
    .address-book-picker-row { width: 100%; display: grid; gap: 3px; text-align: left; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; color: inherit; }
    .address-book-picker-row:hover { border-color: #4c565c; background: #15181b; }
    .recipient-field { display: grid; grid-template-columns: minmax(0, 1fr) auto; gap: 8px; align-items: end; }
    .icon-button { display: inline-grid; place-items: center; width: 38px; height: 38px; padding: 0; line-height: 0; border-radius: 8px; }
    .icon-button svg { width: 19px; height: 19px; stroke: currentColor; stroke-width: 2; fill: none; stroke-linecap: round; stroke-linejoin: round; }
    .modal-delete-button { color: #ffb4b4; }
    input.invalid { border-color: #e36a6a; outline: 2px solid rgba(227,106,106,.16); outline-offset: 2px; }
    .panel-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; margin-bottom: 12px; }
    .panel-head h2, .panel-head h3 { margin-bottom: 0; }
    .switch { display: inline-flex; grid-template-columns: none; align-items: center; gap: 8px; color: #d6dee2; font-weight: 700; }
    .switch input { width: auto; min-width: 0; accent-color: #d5f55f; }
    .wallet-tx-panel { min-width: 0; overflow: hidden; }
    .wallet-tx-panel .panel-head { flex-wrap: wrap; }
    .wallet-tx-filters { display: flex; flex-wrap: wrap; gap: 6px; justify-content: flex-end; align-items: center; }
    .tx-filter { display: inline-flex; align-items: center; gap: 6px; border: 1px solid #3a4248; border-radius: 999px; padding: 4px 8px; color: #9fa8ad; background: #111316; font-size: 12px; font-weight: 850; cursor: pointer; user-select: none; }
    .tx-filter input { position: absolute; width: 1px; height: 1px; min-width: 0; margin: 0; opacity: 0; pointer-events: none; }
    .tx-filter.active { border-color: #d5f55f; background: #202616; color: #d5f55f; }
    .tx-filter:focus-within { outline: 2px solid #d5f55f; outline-offset: 2px; }
    .wallet-tx-list { max-height: min(620px, calc(100vh - 220px)); min-width: 0; display: grid; gap: 8px; overflow-y: auto; overscroll-behavior-y: contain; padding-right: 4px; }
    .wallet-tx-row { position: relative; display: grid; grid-template-columns: minmax(0, 1fr); gap: 8px; align-items: start; border: 1px solid #2f363c; border-radius: 8px; padding: 12px; background: #111316; cursor: pointer; text-align: left; }
    .wallet-tx-row:hover, .wallet-tx-row:focus-visible, .tx-card:hover, .tx-card:focus-visible, .mempool-item:hover, .mempool-item:focus-visible { border-color: #d5f55f; box-shadow: 0 0 0 1px rgba(213, 245, 95, .22); outline: none; }
    .wallet-tx-row.pending { border-color: #3a4147; background: #191c20; box-shadow: inset 3px 0 0 #6f7880; }
    .wallet-tx-row .pill { position: absolute; top: 10px; right: 10px; }
    .wallet-tx-main { display: grid; gap: 5px; min-width: 0; padding-right: 92px; }
    .tx-field { display: grid; grid-template-columns: 74px minmax(0, 1fr); gap: 8px; align-items: baseline; min-width: 0; }
    .tx-label { color: #879198; font-size: 10px; font-weight: 800; text-transform: uppercase; letter-spacing: 0; }
    .tx-value { min-width: 0; color: #dce4e7; font-size: 13px; font-weight: 600; overflow-wrap: anywhere; }
    .tx-value.money { color: #d5f55f; font-variant-numeric: tabular-nums; }
    .tx-value.hash { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; font-weight: 500; }
    .tx-value.number { color: #c7d0d5; font-variant-numeric: tabular-nums; }
    .tx-value.text { color: #e8edf0; }
    .metric-context { display: grid; gap: 5px; margin-top: 12px; }
    .peer-toolbar { display: flex; justify-content: space-between; gap: 12px; align-items: start; flex-wrap: wrap; margin-bottom: 12px; }
    .peer-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(132px, 1fr)); gap: 8px; margin-bottom: 12px; }
    .peer-summary-item { min-width: 0; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .peer-summary-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .peer-summary-value { margin-top: 5px; color: #dce4e7; font-size: 15px; font-weight: 850; }
    .peer-form { display: flex; flex-wrap: wrap; gap: 10px; align-items: end; }
    .peer-form label { min-width: min(320px, 100%); }
    .peer-form input { width: 100%; }
    .peer-status { display: inline-flex; align-items: center; border: 1px solid #3a4248; border-radius: 999px; padding: 3px 8px; color: #a8b2b8; font-size: 11px; font-weight: 850; }
    .peer-status.synced, .peer-status.active { border-color: #566d25; color: #d5f55f; background: #1c2516; }
    .peer-status.stale { border-color: #5f5125; color: #ffe08a; background: #211d12; }
    .peer-status.banned { border-color: #713434; color: #ffb1a8; background: #2a1717; }
    .peer-status.error { border-color: #713434; color: #ffb1a8; background: #2a1717; }
    .peer-actions { display: flex; gap: 6px; align-items: center; }
    .peer-address { display: inline-flex; gap: 7px; align-items: center; flex-wrap: wrap; }
    .country-code { border: 1px solid #3a4248; border-radius: 4px; padding: 1px 5px; color: #a8b2b8; font-size: 10px; font-weight: 850; letter-spacing: .06em; }
    .peer-details { padding: 4px 7px; font-size: 12px; }
    .peer-remove { padding: 4px 7px; border-color: #4f3737; background: #221717; color: #ffb1a8; font-size: 12px; }
    .peer-remove:hover { border-color: #ffb1a8; color: #ffd4cf; }
    .peer-modal-section { display: grid; gap: 8px; margin-top: 14px; }
    .peer-modal-section h3 { margin: 0; color: #8d989f; font-size: 11px; text-transform: uppercase; }
    .peer-modal-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(210px, 1fr)); gap: 8px; }
    .peer-modal-field { min-width: 0; display: grid; gap: 5px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    .peer-modal-field.wide { grid-column: 1 / -1; }
    .peer-modal-value { min-width: 0; overflow-wrap: anywhere; }
    .peer-capabilities { display: flex; flex-wrap: wrap; gap: 6px; }
    .network-health { display: grid; grid-template-columns: minmax(180px, .8fr) minmax(0, 1.2fr); gap: 12px; align-items: stretch; margin-bottom: 12px; }
    .network-health-state { display: grid; align-content: center; gap: 5px; border: 1px solid #3a4248; border-radius: 8px; padding: 12px; background: #111316; }
    .network-health-state.healthy { border-color: #566d25; background: #182112; }
    .network-health-state.syncing, .network-health-state.stale { border-color: #5f5125; background: #211d12; }
    .network-health-state.isolated, .network-health-state.error, .network-health-state.banned { border-color: #713434; background: #241716; }
    .network-health-label { color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .network-health-value { color: #e8edf0; font-size: 20px; font-weight: 900; text-transform: capitalize; }
    .network-health-detail { color: #a8b2b8; font-size: 12px; }
    .network-health-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(116px, 1fr)); gap: 8px; }
    .panel .grid + form { margin-top: 12px; }
    .explorer-shell { width: 100%; display: grid; gap: 12px; }
    .block-rail-wrap { background: #181b1f; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; overflow: hidden; }
    .block-rail-head { display: flex; justify-content: space-between; gap: 10px; align-items: center; margin-bottom: 10px; }
    .block-rail { display: flex; gap: 8px; overflow-x: auto; padding: 1px 0 10px; scroll-snap-type: x proximity; }
    .block-card { flex: 0 0 122px; min-height: 100px; display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 9px; background: #111316; color: #e8edf0; text-align: left; scroll-snap-align: start; }
    button.block-card { cursor: pointer; user-select: none; }
    button.block-card:hover { border-color: #d5f55f; color: #d5f55f; }
    .block-card.selected { background: #202616; border-color: #d5f55f; box-shadow: inset 0 0 0 1px #d5f55f; }
    .block-card.new-block { animation: block-arrive .45s ease both; }
    .block-card-building { border-color: #697b31; border-style: dashed; background: linear-gradient(145deg, #202616, #141912); box-shadow: inset 0 0 18px rgba(213, 245, 95, .05); user-select: none; }
    .block-building-status { width: 7px; height: 7px; margin-top: 5px; border-radius: 50%; background: #d5f55f; box-shadow: 0 0 0 0 rgba(213, 245, 95, .4); animation: building-dot-pulse 1.7s ease-out infinite; }
    @keyframes building-dot-pulse { 0% { box-shadow: 0 0 0 0 rgba(213, 245, 95, .4); } 70%, 100% { box-shadow: 0 0 0 7px rgba(213, 245, 95, 0); } }
    .block-card-select { display: grid; gap: 6px; padding: 0; border: 0; background: transparent; color: inherit; text-align: left; }
    .block-card-head { display: flex; align-items: flex-start; justify-content: space-between; gap: 6px; }
    @keyframes block-arrive { from { opacity: .2; transform: translateX(-12px); } to { opacity: 1; transform: translateX(0); } }
    .block-height { font-size: 18px; font-weight: 900; }
    .block-meta { display: flex; gap: 8px; color: #8d989f; font-size: 12px; }
    .block-miner { width: 100%; padding: 0; border: 0; background: transparent; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11px; text-align: left; overflow-wrap: anywhere; color: #9eb3bc; }
    .block-skeleton-group { flex: 0 0 auto; display: flex; gap: 8px; }
    .block-card-skeleton { grid-template-rows: 22px 18px minmax(28px, 1fr); align-items: start; cursor: default; }
    .skeleton-block-height { width: 46px; height: 22px; border-radius: 6px; background: #30383d; }
    .skeleton-block-meta { display: flex; gap: 6px; min-width: 0; }
    .skeleton-pill { flex: 0 0 auto; width: 18px; height: 14px; border-radius: 999px; background: #2b3136; }
    .skeleton-pill.wide { width: 28px; }
    .skeleton-block-miner { align-self: stretch; min-height: 28px; border-radius: 6px; background: #242a2f; }
    .skeleton-card { pointer-events: none; position: relative; overflow: hidden; }
    .skeleton-card::after { content: ""; position: absolute; inset: 0; background: linear-gradient(90deg, transparent, rgba(213, 245, 95, .12), transparent); animation: skeleton-sweep 1.15s ease-in-out infinite; }
    @keyframes skeleton-sweep { from { transform: translateX(-100%); } to { transform: translateX(100%); } }
    @media (prefers-reduced-motion: reduce) {
      .block-building-status { animation: none; }
    }
    .skeleton-line { height: 12px; border-radius: 6px; background: #2b3136; }
    .skeleton-line.short { width: 42%; }
    .skeleton-line.medium { width: 68%; }
    .skeleton-line.long { width: 88%; }
    .page-sentinel { min-height: 1px; }
    .block-page-sentinel { flex: 0 0 1px; min-height: 100px; }
    .dataset-loader { display: grid; gap: 8px; min-width: 0; }
    .skeleton-table-cell { height: 12px; width: 100%; border-radius: 6px; background: #2b3136; }
    tr.skeleton-card td { padding-top: 11px; padding-bottom: 11px; }
    .detail-grid { display: grid; grid-template-columns: minmax(0, .9fr) minmax(0, 1.1fr); gap: 12px; }
    .detail-kv { display: grid; grid-template-columns: 90px minmax(0, 1fr); gap: 8px; font-size: 13px; margin: 7px 0; }
    .detail-kv .key { color: #8d989f; }
    .detail-link { width: fit-content; max-width: 100%; padding: 0; border: 0; background: transparent; color: #d7f2ff; font: inherit; text-align: left; cursor: pointer; }
    .detail-link code, .detail-link .link-value { color: inherit; text-decoration: underline; text-underline-offset: 3px; }
    .detail-link:hover { color: #d5f55f; }
    .rank-list { display: grid; gap: 8px; }
    .rank-row { display: grid; grid-template-columns: 52px minmax(0, 1fr); gap: 10px; align-items: start; border: 1px solid #30383d; border-radius: 8px; padding: 10px; background: #15191d; }
    .rank-number { color: #d7f2ff; font-weight: 700; }
    .rank-details { display: grid; gap: 6px; min-width: 0; }
    .bundle-summary { display: grid; gap: 5px; border: 1px solid #425027; border-radius: 8px; padding: 12px; margin-bottom: 14px; background: #1b2116; }
    .bundle-summary strong { color: #d5f55f; }
    .bundle-flow { display: grid; grid-template-columns: repeat(auto-fit, minmax(175px, 1fr)); gap: 8px; margin-bottom: 14px; }
    .bundle-slot { display: grid; align-content: start; gap: 8px; min-width: 0; border: 1px solid #30383d; border-radius: 8px; padding: 11px; background: #111316; }
    .bundle-slot-head { display: flex; align-items: center; justify-content: space-between; gap: 8px; }
    .bundle-slot-number { display: inline-grid; place-items: center; width: 26px; height: 26px; border-radius: 50%; background: #d5f55f; color: #111316; font-size: 12px; font-weight: 900; }
    .bundle-slot-status { color: #9fbd42; font-size: 11px; font-weight: 850; text-transform: uppercase; }
    .bundle-slot .tx-field { grid-template-columns: minmax(0, 1fr); gap: 3px; align-items: start; }
    .bundle-slot code { overflow-wrap: anywhere; color: #9eb3bc; font-size: 11px; }
    .bundle-how { display: grid; gap: 8px; border-top: 1px solid #30383d; padding-top: 14px; }
    .bundle-how h3 { margin: 0 0 2px; }
    .bundle-how-step { display: grid; grid-template-columns: 24px minmax(0, 1fr); gap: 8px; align-items: start; color: #cbd3d7; }
    .bundle-how-step > span:first-child { display: inline-grid; place-items: center; width: 22px; height: 22px; border: 1px solid #4a555b; border-radius: 50%; color: #d7f2ff; font-size: 11px; font-weight: 800; }
    .tx-list { display: grid; gap: 8px; }
    .tx-section { display: grid; gap: 8px; }
    .tx-section + .tx-section { margin-top: 10px; padding-top: 10px; border-top: 1px solid #2f363c; }
    .tx-section-title { display: flex; align-items: center; justify-content: space-between; gap: 8px; color: #f4f7f8; font-size: 13px; font-weight: 800; }
    summary.tx-section-title { cursor: pointer; }
    details.tx-section:not([open]) { gap: 0; }
    .tx-section-meta { color: #8e979e; font-size: 12px; font-weight: 600; }
    .tx-scroll-list { display: grid; gap: 8px; max-height: min(430px, calc(100vh - 300px)); min-height: 0; overflow-y: auto; overscroll-behavior-y: contain; padding-right: 4px; }
    .tx-card, .mempool-item { position: relative; display: grid; align-content: start; grid-auto-rows: min-content; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 12px; background: #111316; cursor: pointer; text-align: left; }
    .mempool-item.before-last-block { opacity: .56; }
    .mempool-item.new-since-block { background: #151a12; opacity: 1; }
    .mempool-item.new-since-block::before { content: ""; position: absolute; inset: 0 auto 0 0; width: 3px; border-radius: 8px 0 0 8px; background: #d5f55f; }
    .mempool-state { color: #d5f55f; font-size: 10px; font-weight: 850; text-transform: uppercase; }
    .mempool-time { color: #8d989f; font-size: 11px; font-weight: 700; }
    .mempool-top { display: flex; justify-content: space-between; gap: 8px; align-items: flex-start; min-width: 0; }
    .mempool-top-meta { display: grid; gap: 3px; min-width: 0; }
    .tx-card .pill, .mempool-item .pill { position: absolute; top: 10px; right: 10px; }
    .mempool-item .pill { position: static; flex: 0 0 auto; }
    .pill { display: inline-flex; align-items: center; border-radius: 999px; padding: 2px 8px; font-size: 12px; font-weight: 800; background: #2b3136; color: #d6dee2; }
    .pill.burn { background: #332918; color: #ffd070; }
    .pill.transfer { background: #17312a; color: #8de9cd; }
    .pill.mine { background: #172a34; color: #8bdcff; }
    .pill.reward { background: #263016; color: #d5f55f; }
    .pill.error { background: #341918; color: #ffb1a8; }
    .mempool-panel { min-width: 0; overflow: hidden; }
    .mempool-strip { width: 100%; min-width: 0; display: flex; gap: 8px; overflow-x: auto; overscroll-behavior-x: contain; padding: 1px 0 10px; scroll-snap-type: x proximity; }
    .mempool-item { flex: 0 0 220px; scroll-snap-align: start; align-self: stretch; }
    .tx-modal { width: min(940px, 100%); max-height: calc(100vh - 44px); overflow: auto; border: 1px solid #3b4448; border-radius: 8px; padding: 16px; background: #181b1f; box-shadow: 0 24px 80px rgba(0, 0, 0, .46); }
    .tx-modal-head { display: flex; justify-content: space-between; gap: 16px; align-items: flex-start; margin-bottom: 14px; }
    .tx-modal-title { display: grid; justify-items: start; gap: 6px; min-width: 0; }
    .tx-modal-title h2 { margin: 0; }
    .tx-modal-summary { display: grid; grid-template-columns: repeat(auto-fit, minmax(180px, 1fr)); gap: 8px; margin-bottom: 12px; }
    .utxo-flow { display: grid; grid-template-columns: minmax(0, 1fr) auto minmax(0, 1fr); gap: 12px; align-items: stretch; }
    .utxo-column { display: grid; align-content: start; gap: 8px; min-width: 0; }
    .utxo-column h3 { margin: 0; color: #8d989f; font-size: 11px; text-transform: uppercase; }
    .utxo-node { display: grid; gap: 5px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; min-width: 0; }
    .utxo-node.burned { border-color: #5e4821; background: #1f1a12; }
    .utxo-node.fee { border-color: #4b5260; background: #171a20; }
    .utxo-node.reward { border-color: #526329; background: #1a2013; }
    .utxo-node-label { display: flex; justify-content: space-between; gap: 8px; color: #8d989f; font-size: 11px; font-weight: 800; text-transform: uppercase; }
    .utxo-node-amount { color: #d5f55f; font-weight: 850; font-variant-numeric: tabular-nums; }
    .utxo-node-address, .utxo-node-ref { color: #9eb3bc; font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 12px; overflow-wrap: anywhere; }
    .utxo-arrow { display: grid; place-items: center; color: #d5f55f; font-size: 24px; font-weight: 900; }
    .tx-modal-empty { border: 1px dashed #3a4248; border-radius: 8px; padding: 10px; color: #8d989f; }
    .utxo-list { display: grid; gap: 8px; }
    .wallet-utxo-row { display: grid; gap: 6px; border: 1px solid #2f363c; border-radius: 8px; padding: 10px; background: #111316; }
    @media (max-width: 760px) { .utxo-flow, .mine-action-row, .mine-stats { grid-template-columns: 1fr; } .utxo-arrow { min-height: 28px; transform: rotate(90deg); } .tx-modal-head { align-items: stretch; } }
    @media (max-width: 920px) { .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .network-health { grid-template-columns: 1fr; } }
    @media (max-width: 760px) {
      body { padding-bottom: calc(68px + env(safe-area-inset-bottom)); }
      .app-shell { display: block; min-height: calc(100vh - 68px - env(safe-area-inset-bottom)); min-height: calc(100dvh - 68px - env(safe-area-inset-bottom)); overflow: visible; }
      .sidebar { position: fixed; z-index: 20; inset: auto 0 0; width: 100%; height: auto; display: grid; grid-template-columns: repeat(auto-fit, minmax(34px, 1fr)); gap: 4px; padding: 6px max(8px, env(safe-area-inset-right)) calc(6px + env(safe-area-inset-bottom)) max(8px, env(safe-area-inset-left)); border: 0; border-top: 1px solid #30363b; background: rgba(21, 23, 26, .96); box-shadow: 0 -12px 32px rgba(0, 0, 0, .28); backdrop-filter: blur(14px); }
      .version-panel { display: none; }
      .brand-mark { width: 100%; min-height: 50px; padding: 5px 2px; }
      .side-nav { display: contents; }
      .nav-button { width: 100%; min-width: 0; min-height: 50px; gap: 2px; padding: 5px 2px; }
      .nav-button span { font-size: 10px; }
      .discord-button, .settings-button { width: 100%; min-width: 0; min-height: 50px; height: 50px; gap: 2px; padding: 5px 2px; }
      .sidebar-bottom-actions { display: contents; }
      .settings-button svg { width: 20px; height: 20px; }
      .settings-button span { display: block; }
      .content { min-height: calc(100vh - 68px - env(safe-area-inset-bottom)); min-height: calc(100dvh - 68px - env(safe-area-inset-bottom)); padding: 14px 12px 24px; }
      header, .split, .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .wallet-tx-row { grid-template-columns: 1fr; }
      header { display: flex; align-items: flex-start; padding-bottom: 14px; }
      h1 { font-size: 24px; }
      h2 { font-size: 17px; }
      .header-actions { flex-direction: column-reverse; gap: 6px; align-items: flex-end; text-align: right; font-size: 12px; }
      .basic-status-row { flex-wrap: wrap; }
      .page-title { margin-bottom: 12px; }
      .panel, .metric { padding: 12px; }
      .panel-head { align-items: flex-start; }
      .settings-mode-row { flex-direction: column; align-items: stretch; }
      .settings-mode-row .segmented { width: 100%; }
      .settings-mode-row .segmented button { flex: 1; }
      .metrics-head { align-items: flex-start; flex-direction: column; }
      .metrics-range { width: 100%; }
      .metrics-range button { flex: 1 1 0; }
      .segmented.setup-mode-picker { grid-template-columns: 1fr; }
      .metrics-grid { grid-template-columns: 1fr; }
      form { width: 100%; align-items: stretch; }
      form > label, .burn-fields, .mine-fee-fields, .recipient-field, .amount-field, .send-utxo-summary { width: 100%; }
      .burn-fields, .mine-fee-fields { display: grid; grid-template-columns: 1fr; align-items: stretch; }
      input, textarea { min-width: 0; width: 100%; font-size: 16px; }
      .switch input { width: auto; }
      form > button.primary, .burn-fields > button.primary, .setup-actions > button.primary { min-height: 44px; }
      .peer-toolbar { display: grid; }
      .peer-form { display: grid; grid-template-columns: 1fr; }
      .peer-form label { min-width: 0; }
      .peer-form button { min-height: 44px; }
      .peer-table, .peer-table tbody, .peer-table tr, .peer-table td { display: block; width: 100%; }
      .peer-table { margin: 0; }
      .peer-table thead { display: none; }
      .peer-table tbody { display: grid; gap: 10px; }
      .peer-table tr { overflow: hidden; border: 1px solid #2f363c; border-radius: 8px; background: #111316; }
      .peer-table td { display: grid; grid-template-columns: minmax(82px, .38fr) minmax(0, 1fr); gap: 10px; align-items: baseline; border-bottom: 1px solid #2a3035; padding: 9px 10px; overflow-wrap: anywhere; }
      .peer-table td::before { content: attr(data-label); color: #879198; font-size: 10px; font-weight: 850; text-transform: uppercase; }
      .peer-table td:last-child { border-bottom: 0; }
      .peer-table td[colspan] { display: block; }
      .peer-table td[colspan]::before { display: none; }
      .peer-actions { justify-content: flex-start; }
      .seed-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .block-card { flex-basis: 108px; }
      .setup-overlay { place-items: end center; padding: 8px; }
      .setup-modal, .tx-modal { width: 100%; max-height: calc(100vh - 16px); max-height: calc(100dvh - 16px); border-radius: 12px; padding: 14px; }
      .setup-actions, .danger-actions, .address-book-modal-actions { flex-wrap: wrap; }
      .setup-actions button, .danger-actions button, .address-book-modal-actions button { min-height: 44px; }
      .tx-modal-head { flex-direction: column; }
      .tx-modal-head > button { width: 100%; min-height: 44px; }
      .flash { top: 10px; right: 10px; width: calc(100vw - 20px); }
      .wallet-tx-list, .tx-scroll-list { max-height: none; overflow-y: visible; padding-right: 0; }
      .mining-event { grid-template-columns: auto minmax(0, 1fr); }
      .mining-event-time { grid-column: 2; }
      .dashboard-grid { grid-template-columns: repeat(2, minmax(0, 156px)); }
      .dashboard-card { aspect-ratio: 1; min-height: 0; }
    }
    @media (max-width: 420px) {
      .content { padding-inline: 9px; }
      .sidebar { gap: 2px; padding-inline: 4px; }
      .nav-button, .discord-button, .settings-button { min-height: 48px; }
      .nav-button svg, .discord-button svg, .settings-button svg { width: 19px; height: 19px; }
      .nav-button span, .discord-button span, .settings-button span { font-size: 9px; }
      .panel, .metric, .block-rail-wrap { border-radius: 7px; padding: 10px; }
      .grid, .peer-summary, .network-health-grid, .metrics-summary, .tx-modal-summary { grid-template-columns: 1fr 1fr; }
      .wallet-balance-line { width: 100%; justify-content: space-between; }
      .wallet-tx-main { padding-right: 0; padding-top: 24px; }
      .wallet-tx-row .pill { top: 9px; left: 9px; right: auto; }
      .tx-field, .detail-kv, .stratum-field { grid-template-columns: 1fr; gap: 3px; }
      .seed-grid { grid-template-columns: 1fr; }
    }
  </style>
  <script defer src="/assets/iuna-ui.js?v="#,
    env!("CARGO_PKG_VERSION"),
    r#""></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="iunaApp()" x-init="init()" @keydown.window.escape="closeModals()" x-cloak>
  <div class="app-shell" :inert="syncingNode()">
    <aside class="sidebar" aria-label="iuna navigation">
      <button class="brand-mark" :class="{ active: tab === 'dashboard' }" type="button" @click="setTab('dashboard')" title="Dashboard" aria-label="Dashboard"><svg viewBox="0 0 32 32" aria-hidden="true" focusable="false"><circle class="mark-dot" cx="9.4" cy="7.6" r="2.8"></circle><path class="mark-loop" d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13"></path></svg></button>
      <nav class="side-nav">
        <button class="nav-button" :class="{ active: tab === 'wallet' }" @click="setTab('wallet')" type="button" title="Wallet" aria-label="Wallet">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M3 7h16a2 2 0 0 1 2 2v8a2 2 0 0 1-2 2H3z"></path><path d="M3 7V5a2 2 0 0 1 2-2h12"></path><path d="M16 13h3"></path></svg>
          <span>Wallet</span>
        </button>
        <button class="nav-button" x-show="advancedMode()" :class="{ active: tab === 'mining' }" @click="setTab('mining')" type="button" title="Mining" aria-label="Mining">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 19V5"></path><path d="M4 19h16"></path><path d="M7 15l4-4 3 3 5-7"></path></svg>
          <span>Mining</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'p2p' }" @click="setTab('p2p')" type="button" title="P2P" aria-label="P2P">
          <svg viewBox="0 0 24 24" aria-hidden="true"><circle cx="6" cy="12" r="3"></circle><circle cx="18" cy="6" r="3"></circle><circle cx="18" cy="18" r="3"></circle><path d="M8.5 10.5 15.5 7.5"></path><path d="M8.5 13.5 15.5 16.5"></path></svg>
          <span>P2P</span>
        </button>
        <button class="nav-button" :class="{ active: tab === 'chain' }" @click="setTab('chain')" type="button" title="Explorer" aria-label="Explorer">
          <svg class="chain-icon" viewBox="0 0 24 24" aria-hidden="true"><rect x="1.5" y="9" width="5.5" height="5.5"></rect><rect x="9.25" y="9" width="5.5" height="5.5"></rect><rect x="17" y="9" width="5.5" height="5.5"></rect></svg>
          <span>Chain</span>
        </button>
        <button class="nav-button" x-show="developmentMode()" :class="{ active: tab === 'metrics' }" @click="setTab('metrics')" type="button" title="Metrics" aria-label="Metrics">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 19V5"></path><path d="M4 19h16"></path><path d="M7 15l3-4 3 2 4-7"></path><path d="M7 17h10"></path></svg>
          <span>Metrics</span>
        </button>
        <button class="nav-button" x-show="developmentMode()" :class="{ active: tab === 'leaderboards' }" @click="setTab('leaderboards')" type="button" title="Leaderboards" aria-label="Leaderboards">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M8 4h8v4a4 4 0 0 1-8 0V4Z"></path><path d="M8 6H5v1a4 4 0 0 0 4 4"></path><path d="M16 6h3v1a4 4 0 0 1-4 4"></path><path d="M12 12v4"></path><path d="M8 20h8"></path><path d="M9 16h6v4H9z"></path></svg>
          <span>Leaders</span>
        </button>
      </nav>
      <div class="sidebar-bottom-actions">
        <button class="settings-button" :class="{ active: tab === 'settings' }" type="button" @click="setTab('settings')" title="Settings" aria-label="Settings">
          <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.7 3.2 9.2 5.5a7.2 7.2 0 0 0-1.4.8L5.6 5.6 3.2 9.8l1.7 1.6a7.8 7.8 0 0 0 0 1.6l-1.7 1.6 2.4 4.2 2.2-.7a7.2 7.2 0 0 0 1.4.8l.5 2.3h4.8l.5-2.3a7.2 7.2 0 0 0 1.4-.8l2.2.7 2.4-4.2-1.7-1.6a7.8 7.8 0 0 0 0-1.6L21 9.8l-2.4-4.2-2.2.7a7.2 7.2 0 0 0-1.4-.8l-.5-2.3H9.7Z"></path><circle cx="12" cy="12.2" r="3.1"></circle></svg>
          <span>Settings</span>
        </button>
        <button class="version-panel" type="button" :disabled="desktopUpdateBusy" :class="{ update: updateAvailable(), checking: releaseCheckState === 'checking', failed: releaseCheckState === 'failed' }" :title="versionPanelTitle()" @click="openLatestRelease">
        <span class="version-dot" aria-hidden="true"></span>
        <span class="version-label" x-text="appVersionLabel()"></span>
        <span class="version-update" x-show="updateAvailable()">Update</span>
        </button>
        <a class="discord-button" href="https://discord.gg/wdk8cjkj2G" target="_blank" rel="noopener noreferrer" title="Discord" aria-label="Discord">
        <svg viewBox="0 0 24 24" aria-hidden="true" focusable="false"><path d="M19.54 5.34A16.9 16.9 0 0 0 15.35 4a11.7 11.7 0 0 0-.54 1.1 15.8 15.8 0 0 0-4.62 0A11.7 11.7 0 0 0 9.65 4a16.9 16.9 0 0 0-4.19 1.34C2.81 9.28 2.09 13.12 2.45 16.9A16.8 16.8 0 0 0 7.59 19.5a12.8 12.8 0 0 0 1.1-1.79 10.9 10.9 0 0 1-1.73-.83c.14-.1.28-.21.42-.32a12.1 12.1 0 0 0 9.24 0c.14.11.28.22.42.32-.55.32-1.13.6-1.74.83.32.63.69 1.23 1.1 1.79a16.8 16.8 0 0 0 5.15-2.6c.42-4.38-.72-8.18-2.01-11.56ZM9.32 14.57c-1 0-1.82-.92-1.82-2.04 0-1.13.8-2.05 1.82-2.05 1.02 0 1.84.92 1.82 2.05 0 1.12-.8 2.04-1.82 2.04Zm5.36 0c-1 0-1.82-.92-1.82-2.04 0-1.13.8-2.05 1.82-2.05 1.02 0 1.84.92 1.82 2.05 0 1.12-.8 2.04-1.82 2.04Z"></path></svg>
        <span>Discord</span>
        </a>
      </div>
    </aside>

    <main class="content">
    <header>
      <div>
        <h1 x-text="pageTitle()">iuna</h1>
        <div class="basic-status-row" x-show="basicMode()">
          <span class="basic-status" :class="networkHealthClass()" x-text="basicNetworkStatusLabel()"></span>
          <button class="basic-status-detail" type="button" x-show="basicNetworkNeedsAttention()" @click="setTab('p2p')">Details</button>
        </div>
      </div>
      <div class="header-actions">
        <div class="muted" x-text="lastUpdatedLabel()"></div>
        <button class="lock-button" type="button" x-show="auth.authenticated" @click="logout">Lock</button>
      </div>
    </header>

    <div class="flash" :class="flash?.kind" x-show="flash" x-transition x-text="flash?.message"></div>
    <div class="persistent-banner" x-show="p2pRestartRequired()" x-transition x-text="p2pRestartMessage()"></div>

    <section class="dashboard-section" x-show="tab === 'dashboard'">
      <div class="dashboard-grid">
        <article class="dashboard-card balance good">
          <div class="dashboard-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M4 7.5h14a2 2 0 0 1 2 2v9H4a2 2 0 0 1-2-2v-11a2 2 0 0 1 2-2h13"></path><path d="M15 12h5"></path><circle cx="15" cy="12" r=".5"></circle></svg></div>
          <div class="dashboard-card-copy">
            <div class="dashboard-card-label">Current balance</div>
            <div class="dashboard-card-value">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></div>
          </div>
        </article>

        <article class="dashboard-card" :class="dashboardMiningState()">
          <div class="dashboard-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="m14 13-8.381 8.38a1 1 0 0 1-3.001-3L11 9.999"></path><path d="M15.973 4.027A13 13 0 0 0 5.902 2.373c-1.398.342-1.092 2.158.277 2.601a19.9 19.9 0 0 1 5.822 3.024"></path><path d="M16.001 11.999a19.9 19.9 0 0 1 3.024 5.824c.444 1.369 2.26 1.676 2.603.278A13 13 0 0 0 20 8.069"></path><path d="M18.352 3.352a1.205 1.205 0 0 0-1.704 0l-5.296 5.296a1.205 1.205 0 0 0 0 1.704l2.296 2.296a1.205 1.205 0 0 0 1.704 0l5.296-5.296a1.205 1.205 0 0 0 0-1.704z"></path></svg></div>
          <div class="dashboard-card-copy">
            <div class="dashboard-card-label">Mining</div>
            <div class="dashboard-status" x-text="dashboardMiningLabel()"></div>
          </div>
        </article>

        <article class="dashboard-card burning" :class="dashboardBurnState()">
          <div class="dashboard-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="M12 12c2-3 0-7-1-8 0 3-1.8 4.7-3 6s-2 3.2-2 5a6 6 0 1 0 12 0c0-1.5-1.1-3.9-2-5 0 3-1.7 4-3 4s-2-1-1-2Z"></path></svg></div>
          <div class="dashboard-card-copy">
            <div class="dashboard-card-label">Burning</div>
            <div class="dashboard-status" x-text="dashboardBurnLabel()"></div>
          </div>
        </article>

        <article class="dashboard-card" :class="dashboardBlockState()">
          <div class="dashboard-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><path d="m12 2.8 8 4.4v9.6l-8 4.4-8-4.4V7.2z"></path><path d="m4.3 7.4 7.7 4.3 7.7-4.3M12 11.7v9.1"></path></svg></div>
          <div class="dashboard-card-copy">
            <div class="dashboard-card-label">Last block</div>
            <div class="dashboard-status" x-text="dashboardLastBlockLabel()"></div>
          </div>
        </article>

        <article class="dashboard-card" :class="dashboardNetworkState()">
          <div class="dashboard-card-icon" aria-hidden="true"><svg viewBox="0 0 24 24"><circle cx="5" cy="12" r="2.5"></circle><circle cx="19" cy="6" r="2.5"></circle><circle cx="19" cy="18" r="2.5"></circle><path d="m7.3 11 9.4-4M7.3 13l9.4 4"></path></svg></div>
          <div class="dashboard-card-copy">
            <div class="dashboard-card-label">Network</div>
            <div class="dashboard-status" x-text="basicNetworkStatusLabel()"></div>
          </div>
        </article>
      </div>
    </section>

    <section x-show="tab === 'wallet'">
      <div class="page-title">
        <button class="wallet-balance-line" type="button" @click="openWalletUtxosModal" title="Show wallet UTXOs">
          <span class="tx-label">Balance</span>
          <span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span>
        </button>
      </div>
      <div class="panel" x-show="showOptimizeSuggestion()">
        <h3>Your wallet could be more efficient</h3>
        <p class="panel-description">Your balance consists of <span x-text="walletUtxoPage.total"></span> parts. Combining them can reduce the number of inputs needed for future payments. Review the network fee before deciding.</p>
        <button type="button" @click="openOptimizeWallet">Review optimization</button>
        <button type="button" @click="dismissOptimizeSuggestion">Later</button>
      </div>
      <div class="panel" x-show="status.quantum_migration?.active && status.quantum_migration?.legacy_utxos > 0">
        <h3>Quantum-resistant wallet migration</h3>
        <p class="panel-description">Transaction v2 is active. Review how your legacy Ed25519 balance can move to the hybrid Ed25519 + ML-DSA wallet.</p>
        <div class="detail-grid">
          <div>
            <div class="detail-kv"><div class="key">Legacy balance</div><div>IUNA <span x-text="amountLabel(status.quantum_migration?.legacy_balance || 0)"></span></div></div>
            <div class="detail-kv"><div class="key">Hybrid balance</div><div>IUNA <span x-text="amountLabel(status.quantum_migration?.hybrid_balance || 0)"></span></div></div>
            <div class="detail-kv"><div class="key">Legacy UTXOs</div><div x-text="status.quantum_migration?.legacy_utxos || 0"></div></div>
            <div class="detail-kv"><div class="key">Hybrid address</div><code x-text="status.quantum_migration?.hybrid_address || 'Unlock wallet to derive'"></code></div>
          </div>
          <form @submit.prevent="previewQuantumMigration" x-show="status.quantum_migration?.legacy_balance > 0">
            <label>Fee / byte<input x-model="quantumMigrationFee" type="number" min="0.000001" step="0.000001" required></label>
            <button type="submit" :disabled="quantumMigrationBusy || quantumMigrationSubmitting || status.wallet_locked || status.quantum_migration?.migration_pending" x-text="quantumMigrationBusy ? 'Calculating…' : 'Preview migration'"></button>
            <div class="fee-warning" role="alert" x-show="quantumMigrationError" x-text="quantumMigrationError"></div>
            <div class="muted" x-show="status.quantum_migration?.migration_pending">
              A migration batch is saved in this node's transaction-v2 mempool and will be rebroadcast until confirmation.
              <code x-show="status.quantum_migration?.pending_transaction_id" x-text="status.quantum_migration?.pending_transaction_id || ''"></code>
            </div>
          </form>
        </div>
        <div class="info-copy" x-show="quantumMigrationPreview">
          <p>Review this batch carefully. Migrated value can only be sent to hybrid address-v1 recipients.</p>
          <div class="detail-kv"><div class="key">Inputs</div><div x-text="quantumMigrationPreview?.input_count || 0"></div></div>
          <div class="detail-kv"><div class="key">Transaction size</div><div><span x-text="quantumMigrationPreview?.bytes || 0"></span> bytes</div></div>
          <div class="detail-kv"><div class="key">Network fee</div><div>IUNA <span x-text="amountLabel(quantumMigrationPreview?.fee || 0)"></span></div></div>
          <div class="detail-kv"><div class="key">Amount protected</div><div>IUNA <span x-text="amountLabel(quantumMigrationPreview?.amount || 0)"></span></div></div>
          <div class="detail-kv"><div class="key">Legacy UTXOs after batch</div><div x-text="quantumMigrationPreview?.remaining_legacy_utxos || 0"></div></div>
          <button class="primary" type="button" @click="submitQuantumMigration" :disabled="quantumMigrationSubmitting" x-text="quantumMigrationSubmitting ? 'Submitting…' : 'Confirm migration batch'"></button>
        </div>
      </div>
      <div class="wallet-grid">
        <div class="wallet-actions">
          <div class="panel">
            <h3>Send</h3>
            <form @submit.prevent="sendTransfer">
              <div class="recipient-field">
                <label>Recipient<input x-model="transferTo" @input="transferRecipientChanged" autocomplete="off" required></label>
                <button class="icon-button" type="button" @click="openAddressBookPicker()" title="Choose contact" aria-label="Choose contact">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5.5A2.5 2.5 0 0 1 6.5 3H20v18H6.5A2.5 2.5 0 0 1 4 18.5z"></path><path d="M8 7h8"></path><path d="M8 11h6"></path><path d="M8 15h4"></path></svg>
                </button>
              </div>
              <div class="amount-field">
                <label>Amount<input x-model="transferAmount" @input="scheduleFeeEstimates" type="number" min="0.000001" step="0.000001" required></label>
                <button class="amount-max-button" type="button" @click="setMaxTransferAmount" :disabled="transferMaxDisabled()" title="Use maximum spendable amount">Max</button>
              </div>
              <label>Fee / byte<input x-model="transferFee" @input="scheduleFeeEstimates" type="number" min="0" step="0.000001" required></label>
              <div class="fee-preview" :class="{ error: feeEstimateError('transfer') }" x-text="feeEstimateLabel('transfer')" role="status" aria-live="polite"></div>
              <div class="fee-warning" x-show="feeExceedsAmount('transfer')" x-text="feeExceedsAmountLabel('transfer')" role="status" aria-live="polite"></div>
              <button class="advanced-toggle" type="button" x-show="!hybridTransferRecipient()" @click="toggleSendAdvanced" x-text="showSendAdvanced ? 'Hide UTXOs' : 'UTXOs'"></button>
              <div class="send-utxo-summary" x-show="showSendAdvanced && !hybridTransferRecipient()">
                <div>Selected UTXOs: <span x-text="selectedTransferUtxos.length"></span></div>
                <div>Selected total: IUNA <span x-text="amountLabel(selectedTransferUtxoTotal())"></span></div>
                <div>Required: IUNA <span x-text="amountLabel(transferRequiredTotal())"></span></div>
                <div class="setup-feedback error" x-show="selectedTransferUtxoShortfall() > 0">Selected UTXOs do not cover amount plus fee</div>
                <div class="send-utxo-list">
                  <div class="send-utxo-list-head">
                    <span>UTXOs</span>
                    <span class="send-utxo-actions">
                      <button class="utxo-select-button" type="button" @click="selectAllTransferUtxos" :disabled="walletUtxoPage.loading && walletUtxos.length === 0">Select all</button>
                      <button class="utxo-select-button" type="button" @click="clearTransferUtxos" :disabled="selectedTransferUtxos.length === 0">None</button>
                    </span>
                  </div>
                  <template x-for="utxo in walletUtxos" :key="utxoOutpoint(utxo)">
                    <label class="send-utxo-option" :class="{ disabled: !utxo.spendable }" @click.prevent="toggleTransferUtxoSelection($event, utxo)">
                      <input type="checkbox" :value="utxoOutpoint(utxo)" :checked="selectedTransferUtxos.includes(utxoOutpoint(utxo))" :disabled="!utxo.spendable">
                      <span>
                        <span class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">IUNA <span x-text="amountLabel(utxo.amount)"></span></span></span>
                        <span class="utxo-status" x-show="!utxo.spendable">Pending</span>
                        <code class="tx-value hash" x-text="utxoOutpoint(utxo)"></code>
                      </span>
                    </label>
                  </template>
                  <div class="dataset-loader" x-show="walletUtxoPage.loading" aria-hidden="true">
                    <div class="send-utxo-option skeleton-card"><span><span class="skeleton-line medium"></span><span class="skeleton-line long"></span></span></div>
                    <div class="send-utxo-option skeleton-card"><span><span class="skeleton-line short"></span><span class="skeleton-line long"></span></span></div>
                  </div>
                  <div class="page-sentinel" x-show="walletUtxoPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletUtxo', $el))"></div>
                  <div class="tx-modal-empty" x-show="walletUtxos.length === 0 && !walletUtxoPage.loading">No UTXOs</div>
                </div>
              </div>
              <button class="primary" type="submit" :disabled="sendPreparing || sendConfirmBusy" x-text="sendPreparing ? 'Checking...' : 'Send'"></button>
            </form>
          </div>
          <div class="panel">
            <div class="panel-head">
              <h3>Receive</h3>
              <button type="button" @click="copyReceiveAddress">Copy</button>
            </div>
            <div class="receive-address">
              <div class="muted" x-text="status.quantum_migration?.active ? 'Current hybrid Ed25519 + ML-DSA receive address' : 'Legacy Ed25519 address'"></div>
              <div class="address-box"><code class="wallet-address-link" role="button" tabindex="0" x-text="receiveAddress()" @click="openAddressContact(receiveAddress())" @keydown.enter.prevent="openAddressContact(receiveAddress())" @keydown.space.prevent="openAddressContact(receiveAddress())" title="Add or edit contact"></code></div>
              <div class="muted" x-show="status.quantum_migration?.active">A new receive address is selected after funds are received. Previous addresses remain monitored by this wallet.</div>
              <button class="wallet-address-summary" type="button" x-show="fundedWalletAddresses().length > 0" @click="openWalletAddressesModal">
                <span x-text="walletAddressSummary()"></span>
                <span class="wallet-address-summary-action">View details</span>
              </button>
            </div>
          </div>
          <div class="panel">
            <div class="panel-head">
              <h3>Address Book</h3>
              <div class="address-book-actions">
                <button class="icon-button" type="button" @click="openAddressBookModal()" title="Add contact" aria-label="Add contact">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14"></path><path d="M5 12h14"></path></svg>
                </button>
              </div>
            </div>
            <div class="address-book-list">
              <template x-for="entry in addressBookEntries()" :key="entry.address">
                <button class="address-book-row" type="button" @click="editAddressBookEntry(entry)" :title="`Edit ${entry.name}`">
                  <div>
                    <div class="address-book-name" x-text="entry.name"></div>
                    <code class="tx-value hash" x-text="short(entry.address)"></code>
                  </div>
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9 18l6-6-6-6"></path></svg>
                </button>
              </template>
              <div class="muted" x-show="addressBookEntries().length === 0">No saved addresses</div>
            </div>
          </div>
        </div>
        <div class="panel wallet-tx-panel">
          <div class="panel-head">
            <h3>Transactions</h3>
            <div class="wallet-tx-filters" aria-label="Transaction filters">
              <label class="tx-filter" :class="{ active: walletTxFilters.transfer }">
                <input type="checkbox" x-model="walletTxFilters.transfer" @change="refreshWalletTransactions()">
                <span>Tx</span>
              </label>
              <label class="tx-filter" :class="{ active: walletTxFilters.mine }">
                <input type="checkbox" x-model="walletTxFilters.mine" @change="refreshWalletTransactions()">
                <span>Mine</span>
              </label>
              <label class="tx-filter" :class="{ active: walletTxFilters.burn }">
                <input type="checkbox" x-model="walletTxFilters.burn" @change="refreshWalletTransactions()">
                <span>Burn</span>
              </label>
              <label class="tx-filter" :class="{ active: walletTxFilters.reward }">
                <input type="checkbox" x-model="walletTxFilters.reward" @change="refreshWalletTransactions()">
                <span>Reward</span>
              </label>
            </div>
          </div>
          <div class="wallet-tx-list">
            <template x-for="tx in walletTransactions()" :key="tx.status + '-' + tx.signature">
              <div class="wallet-tx-row" :class="{ pending: tx.status === 'pending' }" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Wallet' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Wallet' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Wallet' })">
                <span class="pill" :class="tx.kind" x-text="tx.direction"></span>
                <div class="wallet-tx-main">
                  <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(tx.amount)"></span></span></div>
                  <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                  <div class="tx-field"><span class="tx-label">Status</span><span class="tx-value text" x-text="txTitle(tx)"></span></div>
                  <div class="tx-field"><span class="tx-label">Time</span><span class="tx-value text" x-text="walletTxTimeLabel(tx)"></span></div>
                  <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" :class="{ 'wallet-address-link': hasWalletAddress(tx.from) }" role="button" :tabindex="hasWalletAddress(tx.from) ? 0 : -1" x-text="shortAddressLabel(tx.from)" @click.stop="openAddressContact(tx.from)" @keydown.enter.stop.prevent="openAddressContact(tx.from)" @keydown.space.stop.prevent="openAddressContact(tx.from)" :title="hasWalletAddress(tx.from) ? 'Add or edit contact' : null"></code></div>
                  <div class="tx-field" x-show="tx.to"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(tx.to)" @click.stop="openAddressContact(tx.to)" @keydown.enter.stop.prevent="openAddressContact(tx.to)" @keydown.space.stop.prevent="openAddressContact(tx.to)" title="Add or edit contact"></code></div>
                  <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                  <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                  <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                </div>
              </div>
            </template>
            <div class="dataset-loader" x-show="walletTxPage.loading" aria-hidden="true">
              <div class="wallet-tx-row skeleton-card"><div class="wallet-tx-main"><div class="skeleton-line medium"></div><div class="skeleton-line short"></div><div class="skeleton-line long"></div></div></div>
              <div class="wallet-tx-row skeleton-card"><div class="wallet-tx-main"><div class="skeleton-line short"></div><div class="skeleton-line medium"></div><div class="skeleton-line long"></div></div></div>
            </div>
            <div class="page-sentinel" x-show="walletTxPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletTx', $el))"></div>
            <div class="muted" x-show="walletTransactions().length === 0 && !walletTxPage.loading">No wallet transactions</div>
          </div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'mining'">
      <div class="page-title">
        <div class="muted">PoB/VDF block production with PoW issuance actions</div>
      </div>
      <div class="mining-grid">
        <div class="panel">
          <h3>Status</h3>
          <div class="mine-stats local-mining-stats" aria-label="Local mining status">
            <div class="mine-stat">
              <div class="mine-stat-label">PoB State</div>
              <div class="mine-stat-value" x-text="pobStatusLabel()"></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">PoB Detail</div>
              <div class="mine-stat-value" x-text="pobDetailLabel()" :title="pobDetailLabel()"></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">PoW State</div>
              <div class="mine-stat-value" x-text="powStatusShortLabel()"></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">Selected Finalizer</div>
              <code class="mine-stat-value" :class="{ 'wallet-address-link': hasWalletAddress(currentFinalizerAddress()) }" role="button" :tabindex="hasWalletAddress(currentFinalizerAddress()) ? 0 : -1" x-text="currentFinalizerLabel()" @click="openAddressContact(currentFinalizerAddress())" @keydown.enter.prevent="openAddressContact(currentFinalizerAddress())" @keydown.space.prevent="openAddressContact(currentFinalizerAddress())" :title="hasWalletAddress(currentFinalizerAddress()) ? 'Add or edit contact' : null"></code>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">Mempool</div>
              <div class="mine-stat-value" x-text="localMiningMempoolLabel()"></div>
            </div>
          </div>
          <div class="mining-event-log" x-show="developmentMode()" aria-label="Mining event log">
            <div class="mining-event-log-head"><span>Event log</span><span x-text="`${miningEventLog().length} lines`"></span></div>
            <template x-if="miningEventLog().length === 0">
              <div class="mining-event-empty skeleton-card" aria-hidden="true"><div class="skeleton-line"></div></div>
            </template>
            <template x-for="event in miningEventLog()" :key="event.key">
              <div class="mining-event" :class="event.kind">
                <span class="mining-event-dot" aria-hidden="true"></span>
                <div>
                  <div class="mining-event-title" x-text="event.title"></div>
                  <div class="mining-event-detail" x-text="event.detail"></div>
                </div>
                <div class="mining-event-time" x-text="event.time"></div>
              </div>
            </template>
          </div>
        </div>
        <div class="panel">
          <div class="panel-head mining-head">
            <h3>Burn</h3>
            <label class="toggle-switch" :class="{ active: miningEnabled }">
              <input type="checkbox" :checked="miningEnabled" @change="setMiningEnabled($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="miningEnabled ? 'On' : 'Off'"></span>
            </label>
          </div>
          <div class="panel-description">Burn IUNA to compete for block finalization. Winning burns finalize PoB/VDF blocks and earn the transaction fees in those blocks.</div>
          <form class="mining-form" @submit.prevent="saveBurn">
            <div class="burn-fields">
              <label>IUNA per block<input x-model="burnAmountDraft" @input="burnAmountDirty = true; scheduleFeeEstimates()" type="number" min="0.000001" step="0.000001"></label>
              <label>Fee / byte<input x-model="burnFeeDraft" @input="burnAmountDirty = true; scheduleFeeEstimates()" type="number" min="0" step="0.000001" required></label>
              <button class="primary" type="submit">Save</button>
            </div>
            <div class="fee-preview" x-text="feeEstimateLabel('burn')"></div>
            <div class="fee-warning" x-show="feeExceedsAmount('burn')" x-text="feeExceedsAmountLabel('burn')" role="status" aria-live="polite"></div>
          </form>
          <div class="mine-stats fee-history" aria-label="Recent block fees">
            <div class="mine-stat">
              <div class="mine-stat-label">Last block fees</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(1))"></span></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">5 block avg</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(5))"></span></div>
            </div>
            <div class="mine-stat">
              <div class="mine-stat-label">30 block avg</div>
              <div class="mine-stat-value money">IUNA <span x-text="amountLabel(recentBlockFeeAverage(30))"></span></div>
            </div>
          </div>
        </div>
        <div class="panel">
          <h3>Mine</h3>
          <div class="panel-description">Search for PoW actions that mint a fixed IUNA reward.</div>
          <div class="mine-settings-form">
            <div class="mine-action-row">
              <div class="mine-stats" aria-label="PoW issuance settings">
                <div class="mine-stat">
                  <div class="mine-stat-label">You receive</div>
                  <div class="mine-stat-value money">IUNA <span x-text="amountLabel(powMineReward())"></span></div>
                </div>
                <div class="mine-stat">
                  <div class="mine-stat-label">Finalizer earns</div>
                  <div class="mine-stat-value">IUNA <span x-text="amountLabel(feeEstimates.mine?.fee ?? 0)"></span></div>
                </div>
                <div class="mine-stat">
                  <div class="mine-stat-label">Difficulty <button class="info-button" type="button" @click="openPowDifficultyInfo" title="How difficulty is adjusted" aria-label="How PoW difficulty is adjusted">i</button></div>
                  <div class="mine-stat-value"><span x-text="powDifficultyLabel()"></span> bits</div>
                </div>
              </div>
              <label class="toggle-switch" :class="{ active: powMiningEnabled }" title="Continuously search for PoW mine actions with a small local work budget">
                <input type="checkbox" :checked="powMiningEnabled" @change="setPowMiningEnabled($event.target.checked)">
                <span class="toggle-track"><span class="toggle-thumb"></span></span>
                <span class="toggle-text" x-text="powMiningEnabled ? 'On' : 'Off'"></span>
              </label>
              <label class="compact-number-field" title="Local PoW worker count">
                <span>Workers</span>
                <input type="number" min="1" :max="maxPowMiningWorkers" :value="powMiningWorkers" @change="setPowMiningWorkers($event.target.value)">
              </label>
            </div>
            <div class="fee-preview" x-text="autoPowStatusLabel()"></div>
          </div>
          <div class="panel-separator" x-show="stratumRuntimeEnabled()"></div>
          <div class="stratum-config" x-show="stratumRuntimeEnabled()">
            <div class="stratum-note">Use the pool URL below in the miner configuration.</div>
            <div class="stratum-fields" aria-label="Stratum settings">
              <div class="stratum-field">
                <div class="stratum-label">Status</div>
                <div class="stratum-value" x-text="status.stratum?.enabled ? 'On' : 'Off'"></div>
              </div>
              <div class="stratum-field">
                <div class="stratum-label">Listener</div>
                <code class="stratum-value hash" x-text="stratumListenAddr()"></code>
              </div>
              <div class="stratum-field">
                <div class="stratum-label">Pool URL</div>
                <code class="stratum-value hash" x-text="stratumPoolUrl()"></code>
              </div>
            </div>
          </div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'p2p'">
      <div class="panel">
        <div class="peer-toolbar">
          <div>
            <h2>Peers</h2>
            <div class="panel-description" x-text="p2pAcceptInbound ? 'Manage outbound peers and inspect inbound or outbound sessions. Public node is accepting inbound P2P connections.' : 'Manage outbound peers and inspect sync health. This node is outbound-only and does not open an inbound P2P port.'"></div>
          </div>
          <form class="peer-form" @submit.prevent="addPeer">
            <label>Peer address<input x-model="peerAddress" placeholder="seed.example:9444"></label>
            <button class="primary" type="submit">Add</button>
          </form>
        </div>
        <div class="network-health" x-show="developmentMode()">
          <div class="network-health-state" :class="networkHealthClass()">
            <div class="network-health-label">Network Health</div>
            <div class="network-health-value" x-text="networkHealth.state || '-'"></div>
            <div class="network-health-detail" x-text="networkHealth.last_chain_payload_error || networkHealth.last_error || 'No peer errors reported'"></div>
          </div>
          <div class="network-health-grid">
            <div class="peer-summary-item"><div class="peer-summary-label">Local Height</div><div class="peer-summary-value" x-text="networkHealth.local_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Tip</div><code class="peer-summary-value" x-text="networkTipLabel()"></code></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Finalized</div><div class="peer-summary-value" x-text="networkHealth.finalized_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Finalized Hash</div><code class="peer-summary-value" x-text="short(networkHealth.finalized_hash)"></code></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Last Block</div><div class="peer-summary-value" x-text="networkLastBlockAgeLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Best Known</div><div class="peer-summary-value" x-text="networkHealth.best_known_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Lag</div><div class="peer-summary-value" x-text="networkLagLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Stale</div><div class="peer-summary-value" x-text="networkHealth.stale_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Banned</div><div class="peer-summary-value" x-text="networkHealth.banned_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Mempool</div><div class="peer-summary-value" x-text="networkHealth.pending_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Plain Tx</div><div class="peer-summary-value" x-text="networkHealth.pending_plain_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">V2 Tx</div><div class="peer-summary-value" x-text="networkHealth.pending_v2_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Next Finalizer</div><code class="peer-summary-value" :class="{ 'wallet-address-link': hasWalletAddress(networkHealth.current_leader) }" role="button" :tabindex="hasWalletAddress(networkHealth.current_leader) ? 0 : -1" x-text="networkFinalizerLabel()" @click="openAddressContact(networkHealth.current_leader)" @keydown.enter.prevent="openAddressContact(networkHealth.current_leader)" @keydown.space.prevent="openAddressContact(networkHealth.current_leader)" :title="hasWalletAddress(networkHealth.current_leader) ? 'Add or edit contact' : null"></code></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Last Mode</div><div class="peer-summary-value" x-text="networkFinalizerModeLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">VDF</div><div class="peer-summary-value" x-text="networkVdfLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Rejected Chain</div><div class="peer-summary-value" x-text="networkHealth.rejected_chain_payloads ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Time Offset</div><div class="peer-summary-value" x-text="networkTimeOffsetLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Clock Warnings</div><div class="peer-summary-value" x-text="networkHealth.bad_clock_peers ?? '-'"></div></div>
          </div>
        </div>
        <div class="peer-summary" x-show="developmentMode()">
          <div class="peer-summary-item"><div class="peer-summary-label">Outbound</div><div class="peer-summary-value" x-text="outboundPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Inbound</div><div class="peer-summary-value" x-text="inboundPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Healthy</div><div class="peer-summary-value" x-text="healthyPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Errors</div><div class="peer-summary-value" x-text="failedPeers().length"></div></div>
          <div class="peer-summary-item"><div class="peer-summary-label">Shared Height</div><div class="peer-summary-value" x-text="sharedHeightLabel()"></div></div>
        </div>
        <div class="table-wrap">
          <table class="peer-table">
            <thead><tr><th>Status</th><th>Address</th><th>Direction</th><th>Last Contact</th><th x-show="developmentMode()">Clock</th><th x-show="developmentMode()">Ban</th><th x-show="developmentMode()">Score</th><th>Height</th><th x-show="developmentMode()">Delta</th><th x-show="developmentMode()">Tip</th><th x-show="developmentMode()">Sent</th><th x-show="developmentMode()">Received</th><th x-show="developmentMode()">Last Error</th><th>Actions</th></tr></thead>
            <tbody>
              <template x-for="peer in peers" :key="peer.address">
                <tr>
                  <td data-label="Status"><span class="peer-status" :class="peerStatus(peer)" x-text="peerStatusLabel(peer)"></span></td>
                  <td data-label="Address"><span class="peer-address"><code x-text="peer.address"></code><span class="country-code" x-show="peer.country_code" x-text="peer.country_code"></span></span></td>
                  <td data-label="Direction" x-text="peer.direction"></td>
                  <td data-label="Last contact" x-text="peerLastContactLabel(peer)"></td>
                  <td data-label="Clock" x-show="developmentMode()" x-text="peerClockLabel(peer)"></td>
                  <td data-label="Ban" x-show="developmentMode()" x-text="peerBanLabel(peer)"></td>
                  <td data-label="Score" x-show="developmentMode()" x-text="peer.misbehavior_score ?? 0"></td>
                  <td data-label="Height" x-text="peer.last_known_height ?? '-'"></td>
                  <td data-label="Delta" x-show="developmentMode()" x-text="peerHeightDelta(peer)"></td>
                  <td data-label="Tip" x-show="developmentMode()"><code x-text="short(peer.last_known_tip_hash)"></code></td>
                  <td data-label="Sent" x-show="developmentMode()" x-text="peer.messages_sent"></td>
                  <td data-label="Received" x-show="developmentMode()" x-text="peer.messages_received"></td>
                  <td data-label="Last error" x-show="developmentMode()" x-text="peer.last_error || ''"></td>
                  <td data-label="Actions"><div class="peer-actions"><button class="peer-details" type="button" @click="openPeerModal(peer)">Details</button><button class="peer-remove" type="button" x-show="canRemovePeer(peer)" @click="removePeer(peer)">Remove</button><span class="muted" x-show="!canRemovePeer(peer)">Observed</span></div></td>
                </tr>
              </template>
              <tr class="skeleton-card" x-show="peerPage.loading" aria-hidden="true">
                <td colspan="14"><div class="skeleton-table-cell"></div></td>
              </tr>
              <tr class="skeleton-card" x-show="peerPage.loading" aria-hidden="true">
                <td colspan="14"><div class="skeleton-table-cell"></div></td>
              </tr>
              <tr x-show="peerPage.hasMore"><td colspan="14"><div class="page-sentinel" x-init="$nextTick(() => observePageSentinel('peer', $el))"></div></td></tr>
              <tr x-show="peers.length === 0 && !peerPage.loading"><td colspan="14">No peers</td></tr>
            </tbody>
          </table>
        </div>
      </div>
      <div class="panel" x-show="developmentMode()">
        <h2>Metrics</h2>
        <div class="grid">
          <div class="metric"><div class="label">Inbound Sessions</div><div class="value" x-text="p2pMetrics.inbound_sessions_started ?? 0"></div></div>
          <div class="metric"><div class="label">Inbound Rejects</div><div class="value" x-text="p2pMetrics.inbound_sessions_rejected ?? 0"></div></div>
          <div class="metric"><div class="label">Outbound Attempts</div><div class="value" x-text="p2pMetrics.outbound_connect_attempts ?? 0"></div></div>
          <div class="metric"><div class="label">Connect Failures</div><div class="value" x-text="p2pMetrics.outbound_connect_failures ?? 0"></div></div>
          <div class="metric"><div class="label">Session Failures</div><div class="value" x-text="p2pMetrics.session_failures ?? 0"></div></div>
          <div class="metric"><div class="label">Parse Errors</div><div class="value" x-text="p2pMetrics.parse_errors ?? 0"></div></div>
          <div class="metric"><div class="label">Empty Frames</div><div class="value" x-text="p2pMetrics.empty_frames ?? 0"></div></div>
          <div class="metric"><div class="label">Self Rejects</div><div class="value" x-text="p2pMetrics.self_peer_rejections ?? 0"></div></div>
          <div class="metric"><div class="label">Self Skips</div><div class="value" x-text="p2pMetrics.self_peer_skips ?? 0"></div></div>
          <div class="metric"><div class="label">Received</div><div class="value" x-text="p2pMetrics.envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Bytes In</div><div class="value" x-text="p2pMetrics.bytes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Status Rx</div><div class="value" x-text="p2pMetrics.peer_status_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Hello Rx</div><div class="value" x-text="p2pMetrics.hello_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Inventory Rx</div><div class="value" x-text="p2pMetrics.inventory_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Data Rx</div><div class="value" x-text="p2pMetrics.data_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Rx</div><div class="value" x-text="p2pMetrics.transactions_received ?? 0"></div></div>
          <div class="metric"><div class="label">Tx Envelopes Rx</div><div class="value" x-text="p2pMetrics.transaction_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Burn Bundles Rx</div><div class="value" x-text="p2pMetrics.burn_bundles_received ?? 0"></div></div>
          <div class="metric"><div class="label">Burn Bundle Envelopes Rx</div><div class="value" x-text="p2pMetrics.burn_bundle_envelopes_received ?? 0"></div></div>
          <div class="metric"><div class="label">Control Rx</div><div class="value" x-text="p2pMetrics.control_envelopes_received ?? 0"></div></div>
        </div>
        <div class="metric-context">
          <div class="tx-field"><span class="tx-label">Last Failure</span><span class="tx-value text" x-text="p2pMetrics.last_session_failure || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Empty</span><span class="tx-value text" x-text="p2pMetrics.last_empty_frame_remote || '-'"></span></div>
          <div class="tx-field"><span class="tx-label">Last Parse</span><span class="tx-value text" x-text="p2pMetrics.last_parse_error || '-'"></span></div>
        </div>
      </div>
    </section>

    <section x-show="tab === 'chain'">
      <div class="explorer-shell">
        <div class="block-rail-wrap">
          <div class="block-rail-head">
            <h2>Blocks</h2>
            <div class="muted"><span x-text="blocks.length"></span> loaded</div>
          </div>
          <div class="block-rail" x-ref="blockRail" @scroll.debounce.200ms="maybeLoadOlderBlocks($event)">
            <div class="block-card block-card-building" role="status" aria-live="polite" aria-label="Block currently being built">
              <div class="block-card-select">
                <div class="block-card-head">
                  <div class="block-height" x-text="buildingBlockHeight()"></div>
                  <div class="block-building-status" aria-hidden="true"></div>
                </div>
                <div class="block-meta">
                  <span x-text="burnCountLabel(buildingBlockOverview())"></span>
                  <span x-text="transferCountLabel(buildingBlockOverview())"></span>
                  <span x-text="mineCountLabel(buildingBlockOverview())"></span>
                </div>
              </div>
              <div class="block-miner" x-text="blockFinalizerLabel(buildingBlockOverview())"></div>
            </div>
            <template x-for="block in blocks" :key="block.hash">
              <button class="block-card" :class="{ selected: selectedBlock?.hash === block.hash, 'new-block': newBlockHashes.has(block.hash) }" @click="selectBlock(block)" type="button" title="Open block details">
                <div class="block-card-select">
                  <div class="block-card-head"><div class="block-height" x-text="block.height"></div></div>
                  <div class="block-meta">
                    <span x-text="burnCountLabel(block)"></span>
                    <span x-text="transferCountLabel(block)"></span>
                    <span x-text="mineCountLabel(block)"></span>
                  </div>
                </div>
                <div class="block-miner" x-text="blockFinalizerLabel(block)"></div>
              </button>
            </template>
            <template x-if="loadingInitialBlocks && blocks.length === 0">
              <div class="block-skeleton-group" aria-hidden="true">
                <div class="block-card block-card-skeleton skeleton-card">
                  <div class="skeleton-block-height"></div>
                  <div class="skeleton-block-meta"><span class="skeleton-pill wide"></span><span class="skeleton-pill"></span><span class="skeleton-pill wide"></span></div>
                  <div class="skeleton-block-miner"></div>
                </div>
                <div class="block-card block-card-skeleton skeleton-card">
                  <div class="skeleton-block-height"></div>
                  <div class="skeleton-block-meta"><span class="skeleton-pill"></span><span class="skeleton-pill wide"></span><span class="skeleton-pill"></span></div>
                  <div class="skeleton-block-miner"></div>
                </div>
                <div class="block-card block-card-skeleton skeleton-card">
                  <div class="skeleton-block-height"></div>
                  <div class="skeleton-block-meta"><span class="skeleton-pill wide"></span><span class="skeleton-pill wide"></span><span class="skeleton-pill"></span></div>
                  <div class="skeleton-block-miner"></div>
                </div>
                <div class="block-card block-card-skeleton skeleton-card">
                  <div class="skeleton-block-height"></div>
                  <div class="skeleton-block-meta"><span class="skeleton-pill"></span><span class="skeleton-pill"></span><span class="skeleton-pill wide"></span></div>
                  <div class="skeleton-block-miner"></div>
                </div>
              </div>
            </template>
            <template x-if="loadingOlder">
              <div class="block-card block-card-skeleton skeleton-card" aria-hidden="true">
                <div class="skeleton-block-height"></div>
                <div class="skeleton-block-meta"><span class="skeleton-pill wide"></span><span class="skeleton-pill"></span><span class="skeleton-pill wide"></span></div>
                <div class="skeleton-block-miner"></div>
              </div>
            </template>
            <div class="page-sentinel block-page-sentinel" x-show="hasMoreBlocks" x-init="$nextTick(() => observeBlockSentinel($el))"></div>
          </div>
        </div>

        <section class="panel">
          <h2>Block Detail</h2>
          <template x-if="selectedBlock">
            <div class="detail-grid">
              <div>
                <div class="detail-kv"><div class="key">Height</div><div x-text="selectedBlock.height"></div></div>
                <div class="detail-kv"><div class="key">Time</div><div x-text="blockTimestampLabel(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Hash</div><code x-text="selectedBlock.hash"></code></div>
                <div class="detail-kv"><div class="key">Previous</div><code x-text="short(selectedBlock.prev_hash)"></code></div>
                <div class="detail-kv">
                  <div class="key">Finalizer</div>
                  <button class="detail-link" type="button" @click="openBurnLeaderRanksModal(selectedBlock)" title="Burn leader ranks">
                    <code x-text="shortAddressLabel(selectedBlock.miner)"></code>
                  </button>
                </div>
                <div class="detail-kv" x-show="selectedBlock.reward_address || selectedBlock.rewardAddress">
                  <div class="key">Reward to</div>
                  <code class="wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(blockRewardAddress(selectedBlock))" @click="openAddressContact(blockRewardAddress(selectedBlock))" @keydown.enter.prevent="openAddressContact(blockRewardAddress(selectedBlock))" @keydown.space.prevent="openAddressContact(blockRewardAddress(selectedBlock))" title="Add or edit contact"></code>
                </div>
                <div class="detail-kv"><div class="key">Mode</div><div x-text="selectedBlock.finalizer_mode === 'recovery' ? 'Recovery' : `Rank ${selectedBlock.finalizer_rank ?? 0}`"></div></div>
                <div class="detail-kv">
                  <div class="key">Reward</div>
                  <button class="detail-link" type="button" @click="openBlockRewardModal(selectedBlock)" title="Show paid and distributed fees" aria-label="Show block reward fee flow">
                    <span class="link-value">IUNA <span x-text="amountLabel(selectedBlock.reward)"></span></span>
                  </button>
                </div>
                <div class="detail-kv">
                  <div class="key">Burn Bundles</div>
                  <button class="detail-link" type="button" @click="openBurnBundleModal(selectedBlock)" title="How the burn bundle attestations were selected" aria-label="Show burn bundle selection details">
                    <span class="link-value" x-text="blockBurnBundleRatio(selectedBlock)"></span>
                  </button>
                </div>
                <div class="detail-kv"><div class="key">Burns</div><div x-text="blockBurnCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Transfers</div><div x-text="blockTransferCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Total Lost</div><div>IUNA <span x-text="amountLabel(blockLostIuna(selectedBlock))"></span></div></div>
                <div class="detail-kv">
                  <div class="key">Storage</div>
                  <button class="detail-link" type="button" @click="openBlockBytesModal(selectedBlock)" title="Compact storage byte breakdown">
                    <span x-text="blockTotalBytes(selectedBlock)"></span>B
                  </button>
                </div>
                <div class="detail-kv"><div class="key">VDF</div><div><span x-text="selectedBlock.vdf_rounds"></span> rounds</div></div>
              </div>
              <div class="tx-list">
                <h3>Transactions</h3>
                <div class="tx-section">
                  <div class="tx-section-title"><span>Envelope</span><span class="tx-section-meta wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(selectedBlock.miner)" @click.stop="openAddressContact(selectedBlock.miner)" @keydown.enter.stop.prevent="openAddressContact(selectedBlock.miner)" @keydown.space.stop.prevent="openAddressContact(selectedBlock.miner)" title="Add or edit contact"></span></div>
                  <div class="tx-scroll-list">
                    <template x-for="tx in selectedBlock.transactions" :key="tx.signature">
                      <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })">
                        <span class="pill" :class="txPillClass(tx)" x-text="txPillLabel(tx)"></span>
                        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" :class="{ 'wallet-address-link': hasWalletAddress(txFrom(tx)) }" role="button" :tabindex="hasWalletAddress(txFrom(tx)) ? 0 : -1" x-text="shortAddressLabel(txFrom(tx))" @click.stop="openAddressContact(txFrom(tx))" @keydown.enter.stop.prevent="openAddressContact(txFrom(tx))" @keydown.space.stop.prevent="openAddressContact(txFrom(tx))" :title="hasWalletAddress(txFrom(tx)) ? 'Add or edit contact' : null"></code></div>
                        <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(txTo(tx))" @click.stop="openAddressContact(txTo(tx))" @keydown.enter.stop.prevent="openAddressContact(txTo(tx))" @keydown.space.stop.prevent="openAddressContact(txTo(tx))" title="Add or edit contact"></code></div>
                        <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                        <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                        <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                      </div>
                    </template>
                  </div>
                  <div class="muted" x-show="selectedBlock.transactions.length === 0">No envelope transactions</div>
                </div>
                <template x-for="bundle in selectedBlock.burn_bundles || selectedBlock.burnBundles || []" :key="bundle.hash">
                  <details class="tx-section">
                    <summary class="tx-section-title"><span x-text="`Burn bundle ${bundle.slot}`"></span><span class="tx-section-meta"><span class="wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(bundle.member)" @click.stop="openAddressContact(bundle.member)" @keydown.enter.stop.prevent="openAddressContact(bundle.member)" @keydown.space.stop.prevent="openAddressContact(bundle.member)" title="Add or edit contact"></span> · <span x-text="bundle.byte_size || bundle.byteSize || 0"></span>B</span></summary>
                    <template x-for="tx in bundle.burns" :key="tx.signature">
                      <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })">
                        <span class="pill" :class="txPillClass(tx)" x-text="txPillLabel(tx)"></span>
                        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" :class="{ 'wallet-address-link': hasWalletAddress(txFrom(tx)) }" role="button" :tabindex="hasWalletAddress(txFrom(tx)) ? 0 : -1" x-text="shortAddressLabel(txFrom(tx))" @click.stop="openAddressContact(txFrom(tx))" @keydown.enter.stop.prevent="openAddressContact(txFrom(tx))" @keydown.space.stop.prevent="openAddressContact(txFrom(tx))" :title="hasWalletAddress(txFrom(tx)) ? 'Add or edit contact' : null"></code></div>
                        <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(txTo(tx))" @click.stop="openAddressContact(txTo(tx))" @keydown.enter.stop.prevent="openAddressContact(txTo(tx))" @keydown.space.stop.prevent="openAddressContact(txTo(tx))" title="Add or edit contact"></code></div>
                        <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
                      </div>
                    </template>
                    <div class="muted" x-show="bundle.burns.length === 0">No burns in bundle</div>
                  </details>
                </template>
                <div class="muted" x-show="selectedBlock.transactions.length === 0 && !(selectedBlock.burn_bundles || selectedBlock.burnBundles || []).length">No transactions</div>
              </div>
            </div>
          </template>
          <div class="muted" x-show="!selectedBlock">Select a block</div>
        </section>

        <section class="panel mempool-panel" x-show="mempool.length > 0 || mempoolPage.loading">
          <h2>Mempool</h2>
          <div class="mempool-strip">
            <template x-for="tx in mempool" :key="tx.signature">
              <div class="mempool-item" :class="mempoolItemClass(tx)" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Mempool' })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Mempool' })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Mempool' })">
                <div class="mempool-top">
                  <div class="mempool-top-meta">
                    <div class="mempool-state" x-show="mempoolItemClass(tx).includes('new-since-block')">New since last block</div>
                    <div class="mempool-time" x-show="mempoolSeenTimeLabel(tx)" x-text="mempoolSeenTimeLabel(tx)"></div>
                  </div>
                  <span class="pill" :class="tx.kind" x-text="tx.kind"></span>
                </div>
                <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" :class="{ 'wallet-address-link': hasWalletAddress(txFrom(tx)) }" role="button" :tabindex="hasWalletAddress(txFrom(tx)) ? 0 : -1" x-text="shortAddressLabel(txFrom(tx))" @click.stop="openAddressContact(txFrom(tx))" @keydown.enter.stop.prevent="openAddressContact(txFrom(tx))" @keydown.space.stop.prevent="openAddressContact(txFrom(tx))" :title="hasWalletAddress(txFrom(tx)) ? 'Add or edit contact' : null"></code></div>
                <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(txTo(tx))" @click.stop="openAddressContact(txTo(tx))" @keydown.enter.stop.prevent="openAddressContact(txTo(tx))" @keydown.space.stop.prevent="openAddressContact(txTo(tx))" title="Add or edit contact"></code></div>
                <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number"><span x-text="txProofBits(tx) ?? '-'"></span> / <span x-text="txDifficultyBits(tx) ?? '-'"></span></span></div>
                <div class="tx-field" x-show="isMineTx(tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="short(txProofHash(tx))"></code></div>
                <div class="tx-field"><span class="tx-label">Signature</span><code class="tx-value hash" x-text="short(tx.signature)"></code></div>
              </div>
            </template>
            <template x-if="mempoolPage.loading">
              <div class="mempool-item skeleton-card" aria-hidden="true">
                <div class="skeleton-line short"></div>
                <div class="skeleton-line medium"></div>
                <div class="skeleton-line long"></div>
              </div>
            </template>
            <div class="page-sentinel" x-show="mempoolPage.hasMore" x-init="$nextTick(() => observePageSentinel('mempool', $el))"></div>
          </div>
        </section>
      </div>
    </section>
    <section x-show="tab === 'metrics'">
      <div class="metrics-shell">
        <div class="metrics-head">
          <div class="segmented metrics-range" role="group" aria-label="Metrics block range">
            <button type="button" :class="{ active: metricsRange === 100 }" @click="setMetricsRange(100)">Last 100</button>
            <button type="button" :class="{ active: metricsRange === 1000 }" @click="setMetricsRange(1000)">Last 1000</button>
            <button type="button" :class="{ active: metricsRange === 'all' }" @click="setMetricsRange('all')">All</button>
          </div>
        </div>
        <div class="metrics-summary">
          <div class="metric"><div class="label">Latest block</div><div class="value" x-text="metricsLatest().height ?? '-'"></div></div>
          <div class="metric"><div class="label">Supply</div><div class="value" x-text="metricAmountLabel(metricsLatest().circulatingSupply)"></div></div>
          <div class="metric"><div class="label">Transactions in block</div><div class="value" x-text="metricsLatest().transactionCount ?? '-'"></div></div>
          <div class="metric"><div class="label">Total UTXOs</div><div class="value" x-text="metricsLatest().utxoCount ?? '-'"></div></div>
          <div class="metric"><div class="label">Total burned</div><div class="value" x-text="metricAmountLabel(metricsLatest().totalBurnedAmount)"></div></div>
          <div class="metric"><div class="label">Difficulty</div><div class="value" x-text="metricsLatest().mineDifficultyBits ?? '-'"></div></div>
        </div>
        <div class="metrics-grid" x-show="(loadingMetrics || metricsPreparing()) && metricsCharts().length === 0">
          <article class="metric-chart-card skeleton-card" aria-hidden="true">
            <div class="metric-chart-head"><div class="skeleton-line medium"></div><div class="skeleton-line short"></div></div>
            <div class="metric-chart-frame"><div class="skeleton-line long"></div></div>
          </article>
          <article class="metric-chart-card skeleton-card" aria-hidden="true">
            <div class="metric-chart-head"><div class="skeleton-line short"></div><div class="skeleton-line medium"></div></div>
            <div class="metric-chart-frame"><div class="skeleton-line long"></div></div>
          </article>
        </div>
        <div class="metrics-empty" x-show="metricsPreparing() && !loadingMetrics">Preparing development mode from the current chain. Metrics will stay updated in the background.</div>
        <div class="metrics-empty" x-show="metricsCharts().length === 0 && !loadingMetrics && !metricsPreparing()">No metrics collected yet</div>
        <div class="metrics-grid">
          <template x-for="chart in metricsCharts()" :key="chart.id">
            <article class="metric-chart-card">
              <div class="metric-chart-head">
                <h3 class="metric-chart-title" x-text="chart.title"></h3>
                <div class="metric-chart-value" x-text="metricLatestValueLabel(chart)"></div>
              </div>
              <div class="metric-chart-frame">
                <div class="metric-chart-y-axis">
                  <template x-for="tick in metricYAxisTicks(chart)" :key="`${chart.id}-y-${tick}`">
                    <span class="metric-chart-axis-label" :style="metricYAxisLabelStyle(chart, tick)" x-text="metricAxisValueLabel(chart, tick)"></span>
                  </template>
                </div>
                <div class="metric-chart-plot" @mousemove="setMetricHoverFromPlot(chart, $event)" @mouseleave="clearMetricHover(chart)">
                  <svg class="metric-chart-svg" viewBox="0 0 300 148" preserveAspectRatio="none" role="img" :aria-label="chart.title">
                    <path class="metric-chart-gridline" :d="metricGridPath(chart)"></path>
                    <line class="metric-chart-axis" x1="4" y1="8" x2="4" y2="132"></line>
                    <line class="metric-chart-axis" x1="4" y1="132" x2="296" y2="132"></line>
                    <polyline class="metric-chart-line" :points="metricChartPoints(chart)"></polyline>
                  </svg>
                  <template x-if="metricHover?.chartId === chart.id">
                    <div class="metric-chart-hover-point" :style="metricHoverPointStyle(chart)" :title="metricTooltipLabel(chart)"></div>
                  </template>
                  <template x-if="metricHover?.chartId === chart.id">
                    <div class="metric-chart-tooltip" :style="metricTooltipStyle(chart)" x-text="metricTooltipLabel(chart)"></div>
                  </template>
                </div>
                <div class="metric-chart-x-axis">
                  <template x-for="tick in metricXAxisTicks(chart)" :key="`${chart.id}-x-${tick}`">
                    <span class="metric-chart-axis-label" :style="metricXAxisLabelStyle(chart, tick)" x-text="`#${tick}`"></span>
                  </template>
                </div>
              </div>
            </article>
          </template>
        </div>
      </div>
    </section>
    <section x-show="developmentMode() && tab === 'leaderboards'">
        <div class="leaderboard-grid">
          <article class="leaderboard-card">
            <div class="leaderboard-tabs" role="tablist" aria-label="Leaderboard">
              <button type="button" role="tab" :aria-selected="leaderboardTab === 'balances'" :class="{ active: leaderboardTab === 'balances' }" @click="leaderboardTab = 'balances'">Balance</button>
              <button type="button" role="tab" :aria-selected="leaderboardTab === 'miners'" :class="{ active: leaderboardTab === 'miners' }" @click="leaderboardTab = 'miners'">Miners</button>
              <button type="button" role="tab" :aria-selected="leaderboardTab === 'burners'" :class="{ active: leaderboardTab === 'burners' }" @click="leaderboardTab = 'burners'">Burners</button>
              <button type="button" role="tab" :aria-selected="leaderboardTab === 'mineProofs'" :class="{ active: leaderboardTab === 'mineProofs' }" @click="leaderboardTab = 'mineProofs'">Proof bits</button>
            </div>
            <template x-if="leaderboardTab !== 'mineProofs'">
              <div>
              <h3 x-text="leaderboardTitle(leaderboardTab)"></h3>
              <div class="leaderboard-list">
                <template x-for="(row, index) in leaderboardRows(leaderboardTab)" :key="`${leaderboardTab}-${row.address}`">
                  <div class="leaderboard-row">
                    <div class="leaderboard-rank" :class="leaderboardRankClass(index)" x-text="leaderboardRankLabel(index)"></div>
                    <div class="leaderboard-main">
                      <code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(row.address)" @click="openAddressContact(row.address)" @keydown.enter.prevent="openAddressContact(row.address)" @keydown.space.prevent="openAddressContact(row.address)" title="Add or edit contact"></code>
                      <div class="muted" x-text="leaderboardCountLabel(leaderboardTab, row)"></div>
                    </div>
                    <div class="leaderboard-amount" x-text="leaderboardAmountLabel(row)"></div>
                  </div>
                </template>
                <template x-for="index in leaderboardPlaceholderRanks(leaderboardRows(leaderboardTab).length)" :key="`${leaderboardTab}-empty-${index}`">
                  <div class="leaderboard-row leaderboard-row-empty" aria-hidden="true">
                    <div class="leaderboard-rank" x-text="`#${index + 1}`"></div>
                    <div class="leaderboard-main">&nbsp;</div>
                    <div class="leaderboard-amount">&nbsp;</div>
                  </div>
                </template>
              </div>
              </div>
            </template>
            <template x-if="leaderboardTab === 'mineProofs'">
              <div>
              <h3>Top 10 Mine Proof Bits</h3>
              <div class="leaderboard-list">
                <template x-for="(row, index) in topMineProofRows()" :key="`${row.proofHash}-${row.height}`">
                  <div class="leaderboard-row">
                    <div class="leaderboard-rank" :class="leaderboardRankClass(index)" x-text="leaderboardRankLabel(index)"></div>
                    <div class="leaderboard-main">
                      <code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="shortAddressLabel(row.address)" @click="openAddressContact(row.address)" @keydown.enter.prevent="openAddressContact(row.address)" @keydown.space.prevent="openAddressContact(row.address)" title="Add or edit contact"></code>
                      <div class="muted">Block #<span x-text="row.height"></span></div>
                    </div>
                    <div class="leaderboard-amount"><span x-text="row.proofBits"></span> bits</div>
                  </div>
                </template>
                <template x-for="index in leaderboardPlaceholderRanks(topMineProofRows().length)" :key="`mine-proofs-empty-${index}`">
                  <div class="leaderboard-row leaderboard-row-empty" aria-hidden="true">
                    <div class="leaderboard-rank" x-text="`#${index + 1}`"></div>
                    <div class="leaderboard-main">&nbsp;</div>
                    <div class="leaderboard-amount">&nbsp;</div>
                  </div>
                </template>
              </div>
              </div>
            </template>
          </article>
        </div>
    </section>
    <section x-show="tab === 'settings'">
      <div class="settings-grid">
        <div class="panel">
          <div class="panel-head">
            <h2>Settings</h2>
            <span class="pill" x-text="advancedMode() ? 'Node mode' : 'Wallet mode'"></span>
          </div>
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Mode</div>
              <div class="muted" x-text="advancedMode() ? 'Node mode shows mining and peer controls.' : 'Wallet mode keeps the interface focused on wallet and chain views.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: advancedMode() }">
              <input type="checkbox" :checked="advancedMode()" @change="setUiMode($event.target.checked ? 'advanced' : 'basic')">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="advancedMode() ? 'Node' : 'Wallet'"></span>
            </label>
          </div>
        </div>
        <div class="panel" x-show="advancedMode()">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Fallback VDF</div>
              <div class="muted">Top <span x-text="recoveryVdfTopRankPercent"></span>% of ticket ranks run fallback work. Recovery remains available to every node.</div>
            </div>
            <label>Top ranks
              <input type="range" min="0" max="100" step="5" :value="recoveryVdfTopRankPercent" @change="setRecoveryVdfTopRankPercent($event.target.value)">
            </label>
          </div>
        </div>
        <div class="panel" x-show="advancedMode()">
          <h3>Node Networking</h3>
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Public node</div>
              <div class="muted" x-text="p2pAcceptInbound ? 'Accepting inbound P2P connections.' : 'Outbound-only P2P; no inbound port is open.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: p2pAcceptInbound }">
              <input type="checkbox" :checked="p2pAcceptInbound" @change="setP2pAcceptInbound($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="p2pAcceptInbound ? 'Public' : 'Private'"></span>
            </label>
          </div>
          <form class="settings-form public-p2p-form" x-show="p2pAcceptInbound" x-transition @submit.prevent="saveP2pAnnounce">
            <label>Bind port<input x-model.number="p2pBindPort" @input="p2pBindPortDirty = true" type="number" min="1" max="65535" step="1" required></label>
            <label>Public P2P address<input x-model="p2pAnnounceAddr" @input="p2pAnnounceDirty = true" placeholder="203.0.113.10:9444"></label>
            <div class="muted">Use this only when TCP port <span x-text="p2pBindPort"></span> is reachable from the internet.</div>
            <div class="setup-actions"><button class="primary" type="submit">Save</button></div>
          </form>
        </div>
        <div class="panel" x-show="advancedMode() && p2pAcceptInbound">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Wallet endpoint</div>
              <div class="muted" x-text="walletEndpointEnabled ? 'Public read and transaction relay API for lightweight wallets.' : 'Public wallet API is disabled.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: walletEndpointEnabled }">
              <input type="checkbox" :checked="walletEndpointEnabled" @change="setWalletEndpointEnabled($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="walletEndpointEnabled ? 'On' : 'Off'"></span>
            </label>
          </div>
          <form class="settings-form" x-show="walletEndpointEnabled" x-transition @submit.prevent="saveWalletEndpointSettings">
            <label>Wallet API port<input x-model.number="walletEndpointBindPort" @input="walletEndpointBindPortDirty = true" type="number" min="1" max="65535" step="1" required></label>
            <div class="muted">This is a separate listener from the management UI. Only forward this port publicly.</div>
            <div class="setup-actions"><button class="primary" type="submit">Save</button></div>
          </form>
        </div>
        <div class="panel" x-show="advancedMode()">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Stratum endpoint</div>
              <div class="muted" x-text="stratumEnabled ? 'ASIC miners can connect after the listener is active.' : 'Stratum listener is disabled.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: stratumEnabled }">
              <input type="checkbox" :checked="stratumEnabled" @change="setStratumEnabled($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="stratumEnabled ? 'On' : 'Off'"></span>
            </label>
          </div>
          <form class="settings-form" x-show="stratumEnabled" x-transition @submit.prevent="saveStratumSettings">
            <label>Bind port<input x-model.number="stratumBindPort" @input="stratumBindPortDirty = true" type="number" min="1" max="65535" step="1" required></label>
            <div class="setup-actions"><button class="primary" type="submit">Save</button></div>
          </form>
        </div>
        <div class="panel">
          <h3>Change Password</h3>
          <div class="setup-feedback" :class="settingsFeedback?.kind" x-show="settingsFeedback" x-transition x-text="settingsFeedback?.message"></div>
          <form class="settings-form" @submit.prevent="changePassword">
            <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
            <label>Current password<input x-model="settingsOldPassword" type="password" autocomplete="current-password" required></label>
            <label>New password<input x-model="settingsNewPassword" type="password" autocomplete="new-password" minlength="12" required></label>
            <label>Confirm new password<input x-model="settingsPasswordConfirm" type="password" autocomplete="new-password" minlength="12" required></label>
            <div class="setup-actions"><button class="primary" type="submit">Change password</button></div>
          </form>
        </div>
        <div class="panel">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Development mode</div>
              <div class="muted" x-text="developmentMode() ? 'Detailed P2P data, metrics, and leaderboards are available.' : 'P2P stays focused on a simple peer list.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: keepTrackOfMetrics }">
              <input type="checkbox" :checked="keepTrackOfMetrics" @change="setKeepTrackOfMetrics($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="developmentMode() ? 'On' : 'Off'"></span>
            </label>
          </div>
        </div>
        <div class="panel danger-panel">
          <h3>Danger Zone</h3>
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title danger-title">Delete local chain</div>
              <div class="danger-copy">Remove the local blockchain database and request a fresh sync from connected peers. Wallet and settings stay on this device.</div>
            </div>
            <button class="danger" type="button" @click="openChainResetModal">Delete chain</button>
          </div>
        </div>
      </div>
    </section>
    </main>
  </div>
  <div class="setup-overlay" x-show="showingNetworkMigration()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="network-migration-title">
    <section class="setup-modal">
      <div class="setup-modal-head">
        <div class="setup-welcome">A new Iuna network is ready 🎉</div>
        <h2 id="network-migration-title">Start from block 0</h2>
        <div class="setup-copy">This version connects to <strong x-text="migrationNetworkLabel()"></strong>. To join it, this node must remove its previous local blockchain and synchronize again.</div>
      </div>
      <div class="panel">
        <h3>Your wallet is separate</h3>
        <p class="muted">Keeping your current wallet preserves its address and recovery phrase. Removing the local chain does not remove your wallet or settings.</p>
        <div class="setup-actions">
          <button class="primary" type="button" :disabled="migrationBusy" @click="finishNetworkMigration(false)" x-text="migrationBusy ? 'Preparing...' : 'Keep wallet and resync'"></button>
          <button class="subtle" type="button" :disabled="migrationBusy" @click="finishNetworkMigration(true)">Use a different wallet</button>
        </div>
        <p class="danger-copy">Choose a different wallet only if you want a new address or need to import another recovery phrase. Without the old recovery phrase, its funds cannot be recovered on your new address.</p>
      </div>
    </section>
  </div>
  <div class="sync-overlay" x-show="syncingNode()" role="status" aria-live="polite" aria-label="Blockchain synchronization in progress">
    <section class="sync-screen">
      <div class="sync-mark" aria-hidden="true"><div class="sync-spinner"></div></div>
      <h1>Synchronizing blockchain</h1>
      <p class="sync-copy">Your node is catching up with the network. The interface will unlock automatically when synchronization is complete.</p>
      <div class="sync-progress" role="progressbar" aria-label="Blockchain sync progress" aria-valuemin="0" aria-valuemax="100" :aria-valuenow="Math.round(syncProgressPercent())">
        <div class="sync-progress-fill" :style="`width: ${syncProgressPercent()}%`"></div>
      </div>
      <div class="sync-progress-label" x-text="syncProgressLabel()"></div>
      <button class="subtle" type="button" @click="openChainResetModal">Sync stuck? Reset local chain</button>
      <p class="muted">This recovery option removes only the local blockchain. Your wallet and settings stay on this device.</p>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedPeerAddress" x-transition.opacity @click.self="closePeerModal()" role="dialog" aria-modal="true" aria-labelledby="peer-details-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="peer-status" :class="peerDetail() ? peerStatus(peerDetail()) : 'pending'" x-text="peerDetail() ? peerStatusLabel(peerDetail()) : 'Unavailable'"></span>
          <h2 id="peer-details-title">Peer details</h2>
          <code x-text="peerDetail()?.address || selectedPeerAddress || '-'"></code>
        </div>
        <button type="button" @click="closePeerModal">Close</button>
      </div>

      <div class="tx-modal-empty" x-show="!peerDetail()">This peer is no longer present in the current peer list.</div>
      <template x-if="peerDetail()">
        <div>
          <section class="peer-modal-section">
            <h3>Connection</h3>
            <div class="peer-modal-grid">
              <div class="peer-modal-field"><span class="tx-label">Direction</span><span class="peer-modal-value" x-text="peerDetail().direction"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Country</span><span class="peer-modal-value" x-text="peerDetail().country_code || '-'"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Last contact</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().last_contact_ms)"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Last success</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().last_success_ms)"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Messages sent</span><span class="peer-modal-value" x-text="peerDetail().messages_sent ?? 0"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Messages received</span><span class="peer-modal-value" x-text="peerDetail().messages_received ?? 0"></span></div>
            </div>
          </section>

          <section class="peer-modal-section">
            <h3>Current chain view</h3>
            <div class="peer-modal-grid">
              <div class="peer-modal-field"><span class="tx-label">Height</span><span class="peer-modal-value" x-text="peerDetail().last_known_height ?? '-'"></span></div>
              <div class="peer-modal-field wide"><span class="tx-label">Tip hash</span><code class="peer-modal-value" x-text="peerDetail().last_known_tip_hash || '-'"></code></div>
            </div>
          </section>

          <section class="peer-modal-section">
            <h3>Last protocol hello</h3>
            <div class="muted">These values are advertised by the remote peer during its handshake.</div>
            <div class="peer-modal-grid">
              <div class="peer-modal-field"><span class="tx-label">Protocol version</span><span class="peer-modal-value" x-text="peerDetail().last_hello?.protocol_version ?? '-'"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Network</span><code class="peer-modal-value" x-text="peerDetail().last_hello?.network_id || '-'"></code></div>
              <div class="peer-modal-field"><span class="tx-label">Advertised address</span><code class="peer-modal-value" x-text="peerDetail().last_hello?.listen_addr || '-'"></code></div>
              <div class="peer-modal-field"><span class="tx-label">Node ID</span><code class="peer-modal-value" x-text="peerDetail().last_hello?.node_id || '-'"></code></div>
              <div class="peer-modal-field"><span class="tx-label">Hello height</span><span class="peer-modal-value" x-text="peerDetail().last_hello?.height ?? '-'"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Peer time</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().last_hello?.time_ms)"></span></div>
              <div class="peer-modal-field wide"><span class="tx-label">Genesis hash</span><code class="peer-modal-value" x-text="peerDetail().last_hello?.genesis_hash || '-'"></code></div>
              <div class="peer-modal-field wide"><span class="tx-label">Hello tip hash</span><code class="peer-modal-value" x-text="peerDetail().last_hello?.tip_hash || '-'"></code></div>
              <div class="peer-modal-field wide">
                <span class="tx-label">Capabilities</span>
                <div class="peer-capabilities" x-show="peerCapabilities(peerDetail()).length">
                  <template x-for="capability in peerCapabilities(peerDetail())" :key="capability"><code class="pill" x-text="capability"></code></template>
                </div>
                <span class="muted" x-show="peerCapabilities(peerDetail()).length === 0">None advertised</span>
              </div>
            </div>
          </section>

          <section class="peer-modal-section">
            <h3>Health and enforcement</h3>
            <div class="peer-modal-grid">
              <div class="peer-modal-field"><span class="tx-label">Clock offset</span><span class="peer-modal-value" x-text="peerClockLabel(peerDetail())"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Clock accepted</span><span class="peer-modal-value" x-text="peerDetail().last_clock_offset_accepted == null ? '-' : (peerDetail().last_clock_offset_accepted ? 'Yes' : 'No')"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Clock observed</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().last_clock_observed_ms)"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Misbehavior score</span><span class="peer-modal-value" x-text="peerDetail().misbehavior_score ?? 0"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Banned for</span><span class="peer-modal-value" x-text="peerBanLabel(peerDetail())"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Banned until</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().banned_until_ms)"></span></div>
              <div class="peer-modal-field"><span class="tx-label">Last error time</span><span class="peer-modal-value" x-text="peerTimestampLabel(peerDetail().last_error_ms)"></span></div>
              <div class="peer-modal-field wide"><span class="tx-label">Last error</span><span class="peer-modal-value" x-text="peerDetail().last_error || '-'"></span></div>
              <div class="peer-modal-field wide"><span class="tx-label">Ban reason</span><span class="peer-modal-value" x-text="peerDetail().ban_reason || '-'"></span></div>
            </div>
          </section>
        </div>
      </template>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="sendConfirmModalOpen" x-transition.opacity @click.self="closeSendConfirmModal()" role="dialog" aria-modal="true" aria-labelledby="send-confirm-title">
    <section class="tx-modal address-book-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill transfer">Send</span>
          <h2 id="send-confirm-title">Confirm transfer</h2>
        </div>
        <button type="button" @click="closeSendConfirmModal" :disabled="sendConfirmBusy">Close</button>
      </div>
      <div class="info-copy">
        <p>Check the full recipient address before sending.</p>
        <div class="info-fact">
          <div class="label">Recipient</div>
          <code class="tx-value hash" x-text="pendingTransfer?.recipient || '-'"></code>
        </div>
        <div class="info-facts">
          <div class="info-fact">
            <div class="label">Amount</div>
            <div class="value">IUNA <span x-text="amountLabel(pendingTransfer?.amount || 0)"></span></div>
          </div>
          <div class="info-fact">
            <div class="label">Fee / byte</div>
            <div class="value">IUNA <span x-text="amountLabel(pendingTransfer?.feePerByte || 0)"></span></div>
          </div>
          <div class="info-fact">
            <div class="label">Transaction size</div>
            <div class="value"><span x-text="pendingTransfer?.bytes || 0"></span> bytes</div>
          </div>
          <div class="info-fact">
            <div class="label">Network fee</div>
            <div class="value">IUNA <span x-text="amountLabel(pendingTransfer?.fee || 0)"></span></div>
          </div>
        </div>
      </div>
      <div class="danger-actions">
        <button class="subtle" type="button" @click="closeSendConfirmModal" :disabled="sendConfirmBusy">Cancel</button>
        <button class="primary" type="button" @click="confirmTransfer" :disabled="sendConfirmBusy" x-text="sendConfirmBusy ? 'Sending...' : 'Confirm and send'"></button>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="desktopUpdateModalOpen" x-transition.opacity @click.self="closeDesktopUpdateModal()" role="dialog" aria-modal="true" aria-labelledby="desktop-update-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill transfer">Update</span>
          <h2 id="desktop-update-title">Install <span x-text="latestReleaseLabel()"></span></h2>
        </div>
        <button type="button" @click="closeDesktopUpdateModal" :disabled="desktopUpdateBusy">Close</button>
      </div>
      <div class="info-copy">
        <p>Iuna will download and verify the signed update, stop the local node, install it, and restart the desktop app.</p>
        <p>Your wallet, settings, and local chain data stay on this device.</p>
      </div>
      <div class="danger-actions">
        <button class="subtle" type="button" @click="closeDesktopUpdateModal" :disabled="desktopUpdateBusy">Later</button>
        <button class="primary" type="button" @click="installDesktopUpdate" :disabled="desktopUpdateBusy" x-text="desktopUpdateBusy ? 'Installing...' : 'Install and restart'"></button>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="chainResetModalOpen" x-transition.opacity @click.self="closeChainResetModal()" role="dialog" aria-modal="true" aria-labelledby="chain-reset-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill error">Danger</span>
          <h2 id="chain-reset-title">Delete local chain</h2>
        </div>
        <button type="button" @click="closeChainResetModal" :disabled="chainResetBusy">Close</button>
      </div>
      <div class="info-copy">
        <p>This deletes the local chain database and clears local chain views. Your wallet and settings stay intact.</p>
        <p>Type <strong>RESET</strong> to confirm.</p>
      </div>
      <form class="settings-form" @submit.prevent="resetLocalChain">
        <label>Confirmation<input x-model="chainResetConfirm" autocomplete="off" spellcheck="false" placeholder="RESET"></label>
        <div class="danger-actions">
          <button class="subtle" type="button" @click="closeChainResetModal" :disabled="chainResetBusy">Cancel</button>
          <button class="danger" type="submit" :disabled="chainResetConfirm.trim() !== 'RESET' || chainResetBusy" x-text="chainResetBusy ? 'Deleting...' : 'Delete and resync'"></button>
        </div>
      </form>
    </section>
  </div>
  <div class="setup-overlay" x-show="showingAuth()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="auth-title">
    <section class="setup-modal auth-form">
      <div class="setup-modal-head">
        <div class="setup-welcome">iuna Access</div>
        <h2 id="auth-title" x-text="auth.configured ? 'Unlock iuna' : 'Set Password'"></h2>
        <div class="setup-copy" x-show="!auth.configured">Choose a local password before wallet setup continues.</div>
        <div class="setup-copy" x-show="auth.configured">Enter the local password to unlock this node.</div>
      </div>
      <div class="setup-feedback" :class="authFeedback?.kind" x-show="authFeedback" x-transition x-text="authFeedback?.message"></div>
      <form x-show="!auth.configured" @submit.prevent="setupPassword">
        <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
        <label>Password<input x-model="authPassword" type="password" autocomplete="new-password" minlength="12" required></label>
        <label>Confirm password<input x-model="authPasswordConfirm" type="password" autocomplete="new-password" minlength="12" required></label>
        <div class="setup-actions"><button class="primary" type="submit">Set password</button></div>
      </form>
      <form x-show="auth.configured && !auth.authenticated" @submit.prevent="login">
        <input class="visually-hidden" type="text" name="username" value="iuna" autocomplete="username" tabindex="-1" aria-hidden="true">
        <label>Password<input x-model="loginPassword" type="password" autocomplete="current-password" required></label>
        <div class="setup-actions"><button class="primary" type="submit">Unlock</button></div>
      </form>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="optimizeOpen" x-transition.opacity @click.self="closeOptimizeWallet()" @keydown.escape.stop="closeOptimizeWallet()" role="dialog" aria-modal="true" aria-labelledby="optimize-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title"><h2 id="optimize-title">Optimize wallet</h2></div>
        <button type="button" @click="closeOptimizeWallet" :disabled="optimizeRunning">Close</button>
      </div>
      <p>Your balance is made up of many small parts. Combining them can make future payments simpler and cheaper.</p>
      <p>Your money stays in your wallet. You only pay the network fee shown below. Parts that would be relatively expensive to combine are skipped.</p>
      <p class="muted">Only available parts are combined. The largest part is kept separate so you can continue making payments.</p>
      <form class="optimize-form" @submit.prevent="previewOptimization">
        <label>Network fee / byte (IUNA)<input type="number" min="0.000001" step="0.000001" x-model="optimizeFee" :disabled="optimizeBusy || optimizeRunning || !!optimizePlan" required></label>
        <label class="optimize-choice"><input type="checkbox" x-model="optimizeMergeRoots" :disabled="optimizeBusy || optimizeRunning || !!optimizePlan"> Also combine different mining groups</label>
        <p class="panel-description">Mining groups are kept separate by default. Combining different groups changes their lineage and can affect burn-committee selection and maturity.</p>
        <button type="submit" :disabled="optimizeBusy || optimizeRunning || !!optimizePlan || status.wallet_locked" x-text="optimizeBusy ? 'Calculating…' : 'Preview costs'"></button>
        <span x-show="status.wallet_locked">Unlock your wallet to preview.</span>
      </form>
      <template x-if="optimizePlan">
        <div>
          <div class="tx-modal-summary" style="margin-top:16px">
            <div class="tx-field"><span class="tx-label">Balance parts (expected)</span><span class="tx-value" x-text="`${optimizePlan.before} → ${optimizePlan.after}`"></span></div>
            <div class="tx-field"><span class="tx-label">Maximum total fee</span><span class="tx-value" x-text="`${amountLabel(optimizePlan.fee)} IUNA`"></span></div>
            <div class="tx-field"><span class="tx-label">Transactions</span><span class="tx-value" x-text="optimizePlan.batches.length"></span></div>
          </div>
          <p class="muted">After confirmation, all batches are submitted automatically. The modal closes when they are queued; confirmation can continue in the background. Fees are fixed by this preview and never raised automatically.</p>
          <p x-show="optimizePlan.batches.length === 0">No suitable batches at this fee and mining-group setting. You can leave your wallet as it is. Small or reserved parts and separate mining groups may remain.</p>
          <p x-show="optimizePlan.batches.length === 32">This preview covers up to 32 batches. You can review another optimization afterwards.</p>
          <button class="primary" type="button" x-show="optimizePlan.batches.length > 0 && !optimizeRunning" @click="runOptimization">Confirm and optimize</button>
          <button type="button" x-show="!optimizeRunning" @click="optimizePlan = null; optimizeError = ''; optimizeMessage = ''">Change settings</button>
        </div>
      </template>
      <p role="status" aria-live="polite" x-text="optimizeMessage"></p>
      <p class="fee-warning" role="alert" x-show="optimizeError" x-text="optimizeError"></p>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showWalletUtxos" x-transition.opacity @click.self="closeWalletUtxosModal()" role="dialog" aria-modal="true" aria-labelledby="wallet-utxos-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="wallet-utxos-title">Wallet UTXOs</h2>
          <button type="button" @click="openOptimizeWallet">Optimize wallet</button>
          <div class="tx-field"><span class="tx-label">Total</span><span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span></div>
        </div>
        <button type="button" @click="closeWalletUtxosModal">Close</button>
      </div>
      <div class="utxo-list">
        <template x-for="utxo in walletUtxos" :key="`${utxo.outpoint.txid}:${utxo.outpoint.index}`">
          <div class="wallet-utxo-row">
            <div class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">IUNA <span x-text="amountLabel(utxo.amount)"></span></span></div>
            <div class="tx-field"><span class="tx-label">Outpoint</span><code class="tx-value hash" x-text="txInputOutpoint({ outpoint: utxo.outpoint })"></code></div>
            <div class="tx-field"><span class="tx-label">Address</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(utxo.address)" @click="openAddressContact(utxo.address)" @keydown.enter.prevent="openAddressContact(utxo.address)" @keydown.space.prevent="openAddressContact(utxo.address)" title="Add or edit contact"></code></div>
          </div>
        </template>
        <div class="dataset-loader" x-show="walletUtxoPage.loading" aria-hidden="true">
          <div class="wallet-utxo-row skeleton-card"><div class="skeleton-line medium"></div><div class="skeleton-line long"></div></div>
          <div class="wallet-utxo-row skeleton-card"><div class="skeleton-line short"></div><div class="skeleton-line long"></div></div>
        </div>
        <div class="page-sentinel" x-show="walletUtxoPage.hasMore" x-init="$nextTick(() => observePageSentinel('walletUtxo', $el))"></div>
        <div class="tx-modal-empty" x-show="walletUtxos.length === 0 && !walletUtxoPage.loading">No wallet UTXOs</div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showWalletAddresses" x-transition.opacity @click.self="closeWalletAddressesModal()" @keydown.escape.stop="closeWalletAddressesModal()" role="dialog" aria-modal="true" aria-labelledby="wallet-addresses-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="wallet-addresses-title">Funded wallet addresses</h2>
          <div class="muted">Previous receive and reward addresses remain monitored automatically.</div>
        </div>
        <button type="button" @click="closeWalletAddressesModal">Close</button>
      </div>
      <div class="wallet-address-list">
        <template x-for="entry in fundedWalletAddresses()" :key="entry.address">
          <div class="wallet-address-row">
            <div class="wallet-address-row-head">
              <span class="wallet-address-state" :class="{ pending: walletAddressHasPendingSpend(entry) }" x-text="walletAddressState(entry)"></span>
              <span class="utxo-node-amount">IUNA <span x-text="amountLabel(entry.balance)"></span></span>
            </div>
            <code class="tx-value hash" x-text="entry.address"></code>
            <div class="muted" x-text="walletAddressUtxoLabel(entry)"></div>
          </div>
        </template>
        <div class="tx-modal-empty" x-show="fundedWalletAddresses().length === 0">No funded wallet addresses</div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="showPowDifficultyInfo" x-transition.opacity @click.self="closePowDifficultyInfo()" role="dialog" aria-modal="true" aria-labelledby="pow-difficulty-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="pow-difficulty-title">PoW Difficulty</h2>
        </div>
        <button type="button" @click="closePowDifficultyInfo">Close</button>
      </div>
      <div class="info-copy">
        <p>Difficulty is adjusted to target about one mine action per block.</p>
        <div class="info-facts">
          <div class="info-fact"><div class="label">Window</div><div class="value">10 blocks</div></div>
          <div class="info-fact"><div class="label">Target</div><div class="value">10 mine actions</div></div>
          <div class="info-fact"><div class="label">Max step</div><div class="value">2 bits</div></div>
        </div>
        <p>If a window includes more mine actions than the target, difficulty rises. If it includes fewer, difficulty falls. The initial difficulty is 12 bits.</p>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedByteBlock" x-transition.opacity @click.self="closeBlockBytesModal()" role="dialog" aria-modal="true" aria-labelledby="block-bytes-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="block-bytes-title" x-text="selectedByteBlock ? `Block ${selectedByteBlock.height} Storage` : 'Block Storage'"></h2>
          <div class="tx-field"><span class="tx-label">Total</span><span class="tx-value number"><span x-text="blockTotalBytes(selectedByteBlock)"></span>B</span></div>
        </div>
        <button type="button" @click="closeBlockBytesModal">Close</button>
      </div>
      <div class="rank-list">
        <template x-for="row in blockByteBreakdown(selectedByteBlock)" :key="row[0]">
          <div class="rank-row">
            <div class="rank-number" x-text="`${row[1]}B`"></div>
            <div class="rank-details">
              <div class="tx-field">
                <span class="tx-label">Category</span>
                <span class="pill" x-show="row[2]" :class="row[2]" x-text="row[0]"></span>
                <span class="tx-value text" x-show="!row[2]" x-text="row[0]"></span>
              </div>
            </div>
          </div>
        </template>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedBurnBundleBlock" x-transition.opacity @click.self="closeBurnBundleModal()" role="dialog" aria-modal="true" aria-labelledby="burn-bundles-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="burn-bundles-title" x-text="selectedBurnBundleBlock ? `Block ${selectedBurnBundleBlock.height} Burn Bundles` : 'Burn Bundles'"></h2>
          <div class="tx-field"><span class="tx-label">Attestations</span><span class="tx-value number" x-text="blockBurnBundleRatio(selectedBurnBundleBlock)"></span></div>
        </div>
        <button type="button" @click="closeBurnBundleModal">Close</button>
      </div>
      <div class="bundle-summary" x-show="blockBurnBundleSlots(selectedBurnBundleBlock).length > 0">
        <strong><span x-text="blockBurnBundleRatio(selectedBurnBundleBlock)"></span> attestations committed</strong>
        <span class="muted">The first number is the attestations used by this block. The second is the committed committee represented in the block—not every candidate wallet considered by the network.</span>
      </div>
      <div class="bundle-flow" x-show="blockBurnBundleSlots(selectedBurnBundleBlock).length > 0">
        <template x-for="slot in blockBurnBundleSlots(selectedBurnBundleBlock)" :key="`${selectedBurnBundleBlock.hash}-${slot.slot}`">
          <div class="bundle-slot">
            <div class="bundle-slot-head">
              <span class="bundle-slot-number" x-text="slot.slot"></span>
              <span class="bundle-slot-status">Included</span>
            </div>
            <div class="tx-field"><span class="tx-label" x-text="slot.role"></span><code class="wallet-address-link" role="button" tabindex="0" x-text="addressLabel(slot.member)" @click="openAddressContact(slot.member)" @keydown.enter.prevent="openAddressContact(slot.member)" @keydown.space.prevent="openAddressContact(slot.member)" title="Add or edit contact"></code></div>
            <div class="tx-field" x-show="slot.rewardAddress"><span class="tx-label">Reward to</span><code class="wallet-address-link" role="button" tabindex="0" x-text="addressLabel(slot.rewardAddress)" @click="openAddressContact(slot.rewardAddress)" @keydown.enter.prevent="openAddressContact(slot.rewardAddress)" @keydown.space.prevent="openAddressContact(slot.rewardAddress)" title="Add or edit contact"></code></div>
            <div class="tx-field"><span class="tx-label">Contribution</span><span class="tx-value text" x-text="burnBundleSlotDetail(slot)"></span></div>
            <div class="tx-field" x-show="slot.hash"><span class="tx-label">Bundle</span><code x-text="short(slot.hash)"></code></div>
          </div>
        </template>
      </div>
      <div class="tx-modal-empty" x-show="blockBurnBundleSlots(selectedBurnBundleBlock).length === 0">Recovery blocks do not require burn-bundle attestations.</div>
      <div class="bundle-how" x-show="blockBurnBundleSlots(selectedBurnBundleBlock).length > 0">
        <h3>How are they chosen?</h3>
        <div class="bundle-how-step"><span>1</span><span><strong>Finalizer:</strong> the burn-ticket ranking chooses the block finalizer, which implicitly fills slot 0.</span></div>
        <div class="bundle-how-step"><span>2</span><span><strong>Root groups:</strong> mature UTXO lineage weight selects independent groups for the additional committee slots. One root can win at most one slot.</span></div>
        <div class="bundle-how-step"><span>3</span><span><strong>Wallet:</strong> a wallet with a valid ticket represents each winning root group. Its burn amount does not increase that root's committee weight.</span></div>
        <div class="bundle-how-step"><span>4</span><span><strong>Burn list:</strong> each member bundles valid pending burns by highest absolute fee, using the signature as the deterministic tie-breaker.</span></div>
        <p class="muted">This block records the included members and attestations. Historical lineage weights and candidates that were not included are not stored in the block.</p>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedBurnLeaderBlock" x-transition.opacity @click.self="closeBurnLeaderRanksModal()" role="dialog" aria-modal="true" aria-labelledby="burn-ranks-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="burn-ranks-title" x-text="burnLeaderRanksTitle(selectedBurnLeaderBlock)"></h2>
          <div class="tx-field"><span class="tx-label">Finalizer</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="selectedBurnLeaderBlock ? addressLabel(selectedBurnLeaderBlock.miner) : '-'" @click="openAddressContact(selectedBurnLeaderBlock?.miner)" @keydown.enter.prevent="openAddressContact(selectedBurnLeaderBlock?.miner)" @keydown.space.prevent="openAddressContact(selectedBurnLeaderBlock?.miner)" title="Add or edit contact"></code></div>
          <div class="tx-field" x-show="selectedBurnLeaderBlock?.reward_address || selectedBurnLeaderBlock?.rewardAddress"><span class="tx-label">Reward to</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(blockRewardAddress(selectedBurnLeaderBlock))" @click="openAddressContact(blockRewardAddress(selectedBurnLeaderBlock))" @keydown.enter.prevent="openAddressContact(blockRewardAddress(selectedBurnLeaderBlock))" @keydown.space.prevent="openAddressContact(blockRewardAddress(selectedBurnLeaderBlock))" title="Add or edit contact"></code></div>
        </div>
        <button type="button" @click="closeBurnLeaderRanksModal">Close</button>
      </div>
      <div class="rank-list">
        <div class="dataset-loader" x-show="burnLeaderRanksLoading(selectedBurnLeaderBlock)" role="status" aria-label="Loading burn leader ranks">
          <div class="rank-row skeleton-card" aria-hidden="true"><div class="skeleton-line short"></div><div class="rank-details"><div class="skeleton-line medium"></div><div class="skeleton-line long"></div><div class="skeleton-line short"></div></div></div>
          <div class="rank-row skeleton-card" aria-hidden="true"><div class="skeleton-line short"></div><div class="rank-details"><div class="skeleton-line long"></div><div class="skeleton-line medium"></div><div class="skeleton-line short"></div></div></div>
        </div>
        <div class="tx-modal-empty" x-show="burnLeaderRanksError" x-text="burnLeaderRanksError" role="alert"></div>
        <template x-for="rank in burnLeaderRanks(selectedBurnLeaderBlock)" :key="`${selectedBurnLeaderBlock.hash}-${rank.rank}-${rank.ticket_id ?? rank.ticketId}`">
          <div class="rank-row">
            <div class="rank-number" x-text="burnLeaderRankLabel(rank)"></div>
            <div class="rank-details">
              <div class="tx-field"><span class="tx-label">Owner</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(rank.owner)" @click="openAddressContact(rank.owner)" @keydown.enter.prevent="openAddressContact(rank.owner)" @keydown.space.prevent="openAddressContact(rank.owner)" title="Add or edit contact"></code></div>
              <div class="tx-field"><span class="tx-label">Burn</span><span class="tx-value money">IUNA <span x-text="amountLabel(rank.amount)"></span></span></div>
              <div class="tx-field"><span class="tx-label">Ticket</span><code class="tx-value hash" x-text="short(rank.ticket_id ?? rank.ticketId)"></code></div>
              <div class="tx-field"><span class="tx-label">Eligible</span><span class="tx-value number" x-text="burnLeaderEligibilityLabel(rank)"></span></div>
            </div>
          </div>
        </template>
        <div class="tx-modal-empty" x-show="!burnLeaderRanksError && !burnLeaderRanksLoading(selectedBurnLeaderBlock) && burnLeaderRanks(selectedBurnLeaderBlock).length === 0">No burn leader ranks</div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="selectedTransaction" x-transition.opacity @click.self="closeTransactionModal()" role="dialog" aria-modal="true" aria-labelledby="tx-modal-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill" :class="txPillClass(selectedTransaction?.tx)" x-text="txPillLabel(selectedTransaction?.tx)"></span>
          <h2 id="tx-modal-title">Transaction</h2>
          <code class="tx-value hash" x-text="selectedTransaction?.tx?.signature || '-'"></code>
        </div>
        <button type="button" @click="closeTransactionModal">Close</button>
      </div>
      <div class="tx-modal-summary">
        <div class="tx-field"><span class="tx-label">Source</span><span class="tx-value text" x-text="selectedTransactionLabel()"></span></div>
        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(selectedTransaction?.tx || {}))"></span></span></div>
        <div class="tx-field" x-show="!isRewardTx(selectedTransaction?.tx)"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(selectedTransaction?.tx)"></span></div>
        <div class="tx-field" x-show="!isRewardTx(selectedTransaction?.tx)"><span class="tx-label">From</span><code class="tx-value hash" :class="{ 'wallet-address-link': hasWalletAddress(txFrom(selectedTransaction?.tx || {})) }" role="button" :tabindex="hasWalletAddress(txFrom(selectedTransaction?.tx || {})) ? 0 : -1" x-text="addressLabel(txFrom(selectedTransaction?.tx || {}))" @click="openAddressContact(txFrom(selectedTransaction?.tx || {}))" @keydown.enter.prevent="openAddressContact(txFrom(selectedTransaction?.tx || {}))" @keydown.space.prevent="openAddressContact(txFrom(selectedTransaction?.tx || {}))" :title="hasWalletAddress(txFrom(selectedTransaction?.tx || {})) ? 'Add or edit contact' : null"></code></div>
        <div class="tx-field" x-show="!isRewardTx(selectedTransaction?.tx) && txTo(selectedTransaction?.tx || {})"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(txTo(selectedTransaction?.tx || {}))" @click="openAddressContact(txTo(selectedTransaction?.tx || {}))" @keydown.enter.prevent="openAddressContact(txTo(selectedTransaction?.tx || {}))" @keydown.space.prevent="openAddressContact(txTo(selectedTransaction?.tx || {}))" title="Add or edit contact"></code></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Difficulty</span><span class="tx-value number" x-text="txDifficultyBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number" x-text="txProofBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="txProofHash(selectedTransaction?.tx) || '-'"></code></div>
      </div>
      <div class="utxo-flow">
        <div class="utxo-column">
          <h3>Inputs</h3>
          <template x-for="(input, index) in txInputs(selectedTransaction?.tx || {})" :key="txInputKey(input, index)">
            <div class="utxo-node">
              <div class="utxo-node-label"><span x-text="input.rewardFee ? `Paid fee ${index + 1}` : `Input ${index + 1}`"></span><span x-text="input.rewardFee ? input.transactionKind : 'spent'"></span></div>
              <div class="utxo-node-ref" x-text="txInputOutpoint(input)"></div>
              <div class="tx-field"><span class="tx-label">Value</span><span class="tx-value money" x-text="txInputAmountLabel(input)"></span></div>
              <div class="tx-field" x-show="input.owner"><span class="tx-label" x-text="input.rewardFee ? 'Paid by' : 'Owner'"></span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(input.owner)" @click="openAddressContact(input.owner)" @keydown.enter.prevent="openAddressContact(input.owner)" @keydown.space.prevent="openAddressContact(input.owner)" title="Add or edit contact"></code></div>
              <div class="tx-field"><span class="tx-label" x-text="input.rewardFee ? 'Transaction' : 'Sig'"></span><code class="tx-value hash" x-text="short(input.signature)"></code></div>
            </div>
          </template>
          <div class="tx-modal-empty" x-show="txInputs(selectedTransaction?.tx || {}).length === 0">No inputs</div>
        </div>
        <div class="utxo-arrow" aria-hidden="true">&rarr;</div>
        <div class="utxo-column">
          <h3>Outputs</h3>
          <template x-for="(output, index) in txVisualOutputs(selectedTransaction?.tx || {})" :key="txOutputKey(output, index)">
            <div class="utxo-node" :class="{ burned: output.kind === 'burned', fee: output.kind === 'fee', reward: output.kind === 'reward' }">
              <div class="utxo-node-label"><span x-text="output.label"></span><span x-text="output.kind"></span></div>
              <div class="utxo-node-amount">IUNA <span x-text="amountLabel(output.amount)"></span></div>
              <template x-if="output.address">
                <div class="tx-field"><span class="tx-label">To</span><code class="tx-value hash wallet-address-link" role="button" tabindex="0" x-text="addressLabel(output.address)" @click="openAddressContact(output.address)" @keydown.enter.prevent="openAddressContact(output.address)" @keydown.space.prevent="openAddressContact(output.address)" title="Add or edit contact"></code></div>
              </template>
              <template x-if="output.detail">
                <div class="tx-field"><span class="tx-label" x-text="output.detailLabel"></span><code class="tx-value hash" x-text="output.detail"></code></div>
              </template>
            </div>
          </template>
          <div class="tx-modal-empty" x-show="txVisualOutputs(selectedTransaction?.tx || {}).length === 0">No outputs</div>
        </div>
      </div>
    </section>
  </div>
  <div class="setup-overlay transaction-overlay" x-show="addressBookPickerOpen" x-transition.opacity @click.self="closeAddressBookPicker()" role="dialog" aria-modal="true" aria-labelledby="address-book-picker-title">
    <section class="tx-modal address-book-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <span class="pill" x-text="addressBookStandalone ? 'Contact' : 'Send'"></span>
          <h2 id="address-book-picker-title" x-text="addressBookStandalone ? (addressBookEditingAddress ? 'Edit Contact' : 'Add Contact') : 'Choose Contact'"></h2>
        </div>
        <div class="address-book-actions">
          <button class="icon-button" type="button" x-show="!addressBookModalOpen" @click="openAddressBookModal(null, false)" title="Add contact" aria-label="Add contact">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M12 5v14"></path><path d="M5 12h14"></path></svg>
          </button>
          <button class="icon-button" type="button" @click="closeAddressBookPicker()" title="Close" aria-label="Close">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M18 6 6 18"></path><path d="m6 6 12 12"></path></svg>
          </button>
        </div>
      </div>
      <div class="address-book-picker-list" x-show="!addressBookModalOpen">
        <template x-for="entry in addressBookEntries()" :key="entry.address">
          <button class="address-book-picker-row" type="button" @click="selectTransferContact(entry.address)" :title="`Send to ${entry.name}`">
            <span class="address-book-name" x-text="entry.name"></span>
            <code class="tx-value hash" x-text="short(entry.address)"></code>
          </button>
        </template>
        <div class="tx-modal-empty" x-show="addressBookEntries().length === 0">No saved addresses</div>
      </div>
      <form class="address-book-form" x-show="addressBookModalOpen" @submit.prevent="saveAddressBookEntry">
        <label>Name<input x-model="addressBookDraftName" autocomplete="off" required></label>
        <label>Address<input x-model="addressBookDraftAddress" autocomplete="off" required :class="{ invalid: addressBookDraftAddress && !validAddressBookAddress(addressBookDraftAddress) }"></label>
        <div class="setup-feedback error" x-show="addressBookDraftAddress && !validAddressBookAddress(addressBookDraftAddress)">Address must be a Bech32m address for this network</div>
        <div class="address-book-modal-actions">
          <button class="icon-button modal-delete-button" type="button" x-show="addressBookEditingAddress" @click="removeAddressBookEntry({ address: addressBookEditingAddress, name: addressBookDraftName || addressBookEditingAddress })" title="Delete contact" aria-label="Delete contact">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16"></path><path d="M10 11v6"></path><path d="M14 11v6"></path><path d="M6 7l1 14h10l1-14"></path><path d="M9 7V4h6v3"></path></svg>
          </button>
          <button class="icon-button" type="button" @click="closeAddressBookModal()" :title="addressBookStandalone ? 'Cancel' : 'Back to contacts'" :aria-label="addressBookStandalone ? 'Cancel' : 'Back to contacts'">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M15 18l-6-6 6-6"></path></svg>
          </button>
          <button class="primary icon-button" type="submit" title="Save contact" aria-label="Save contact">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M19 21H5a2 2 0 0 1-2-2V5a2 2 0 0 1 2-2h11l5 5v11a2 2 0 0 1-2 2Z"></path><path d="M7 3v6h8"></path><path d="M7 21v-8h10v8"></path></svg>
          </button>
        </div>
      </form>
    </section>
  </div>
  <div class="setup-overlay" x-show="showingSetup()" x-transition.opacity role="dialog" aria-modal="true" aria-labelledby="setup-title">
    <section class="setup-modal">
      <div class="setup-modal-head">
        <div class="setup-welcome">Welcome to iuna</div>
        <h2 id="setup-title">Initial Setup</h2>
        <div class="setup-copy">Connect this node to the network, then set up the local wallet.</div>
      </div>
      <div class="setup-feedback" :class="setupFeedback?.kind" x-show="setupFeedback" x-transition x-text="setupFeedback?.message"></div>
      <div class="setup-grid">
        <div class="setup-section setup-node-mode">
          <div class="panel-head">
            <h3>Mode</h3>
            <span class="pill">Change later in Settings</span>
          </div>
          <div class="segmented setup-mode-picker" role="tablist" aria-label="Initial node mode">
            <button type="button" :class="{ active: setupNodeMode === 'wallet' }" @click="selectSetupNodeMode('wallet')">Wallet</button>
            <button type="button" :class="{ active: setupNodeMode === 'non-listening' }" @click="selectSetupNodeMode('non-listening')">Non-listening node</button>
            <button type="button" :class="{ active: setupNodeMode === 'listening' }" @click="selectSetupNodeMode('listening')">Listening node</button>
          </div>
          <div class="setup-network-copy" x-text="setupNodeModeCopy()"></div>
        </div>
        <div class="setup-section setup-network">
          <div class="panel-head">
            <h3>Network</h3>
            <a class="setup-network-link" href="https://getiuna.org/git/iuna/file/KNOWN_NODES.txt.html" target="_blank" rel="noreferrer">Known nodes</a>
          </div>
          <div class="setup-network-row">
            <label><span x-text="setupRequiresPeer() ? 'Bootstrap peer (required)' : 'Bootstrap peer'"></span><input x-model="setupPeerAddress" placeholder="iuna.jhx.app:9444"></label>
            <label x-show="setupNodeMode === 'listening'" x-transition>Bind port<input x-model.number="p2pBindPort" @input="p2pBindPortDirty = true" type="number" min="1" max="65535" step="1" required></label>
          </div>
          <div class="setup-network-copy" x-text="setupRequiresPeer() ? 'A bootstrap peer is required before this node can join the network. Known nodes help discovery; they do not control your wallet or decide valid blocks.' : 'You can add a bootstrap peer now or later from the P2P screen. Known nodes help discovery; they do not control your wallet or decide valid blocks.'"></div>
        </div>
        <div class="setup-section setup-wallet-section seed-panel">
          <div class="panel-head">
            <h3>Wallet</h3>
          </div>
          <div class="segmented" role="tablist" aria-label="Wallet setup mode">
            <button type="button" :class="{ active: setupWalletMode === 'create' }" @click="selectSetupWalletMode('create')">Create</button>
            <button type="button" :class="{ active: setupWalletMode === 'import' }" @click="selectSetupWalletMode('import')">Import</button>
          </div>
          <div x-show="setupWalletMode === 'create'" class="seed-panel">
            <div class="setup-field">
              <div class="setup-field-label">Address</div>
              <div class="address-box setup-address-box">
                <code class="wallet-address-link" role="button" tabindex="0" x-text="setupAddress()" @click="openAddressContact(setupAddress())" @keydown.enter.prevent="openAddressContact(setupAddress())" @keydown.space.prevent="openAddressContact(setupAddress())" title="Add or edit contact"></code>
                <button type="button" @click="copyAddress">Copy</button>
              </div>
            </div>
            <template x-if="setupSeedWords().length > 0 && setupSeedStep === 'write'">
              <div class="seed-panel">
                <div class="setup-field">
                  <div class="setup-field-label">Recovery phrase</div>
                  <div class="seed-grid">
                    <template x-for="(word, index) in setupSeedWords()" :key="index">
                      <div class="seed-word">
                        <span class="index" x-text="index + 1"></span>
                        <span class="word" x-text="word"></span>
                      </div>
                    </template>
                  </div>
                </div>
                <div class="setup-actions">
                  <button type="button" class="subtle" @click="generateSetupSeed">Regenerate</button>
                  <button type="button" class="subtle" x-show="setupWallet.dev_verify_bypass" @click="skipSeedVerificationForDev">Skip verification</button>
                  <button type="button" class="primary" @click="beginSeedVerification">I wrote it down</button>
                </div>
              </div>
            </template>
            <template x-if="setupSeedWords().length === 0">
              <div class="seed-panel">
                <div class="muted">This wallet does not have a recovery phrase yet.</div>
                <button type="button" class="primary" @click="generateSetupSeed">Generate recovery phrase</button>
              </div>
            </template>
            <template x-if="setupSeedStep === 'verify'">
              <div class="seed-panel">
                <div class="verify-grid">
                  <template x-for="challenge in verifyChallenges" :key="challenge.index">
                    <label>
                      <span>Word <span x-text="challenge.position"></span></span>
                      <input x-model="verifyAnswers[challenge.index]" autocomplete="off">
                    </label>
                  </template>
                </div>
                <div class="setup-actions">
                  <button type="button" class="subtle" @click="setupSeedStep = 'write'">Back</button>
                  <button type="button" class="primary" @click="verifyGeneratedSeed">Verify</button>
                </div>
              </div>
            </template>
            <template x-if="setupSeedStep === 'verified' && walletVerified">
              <div class="setup-status">Recovery phrase verified</div>
            </template>
          </div>
          <div x-show="setupWalletMode === 'import'" class="seed-panel">
            <form @submit.prevent="importSetupSeed">
              <label>Recovery phrase<textarea x-model="importSeedPhrase" autocomplete="off" spellcheck="false" placeholder="24 words, separated by spaces or new lines"></textarea></label>
              <button class="primary" type="submit">Import</button>
            </form>
            <template x-if="walletVerified">
              <div class="setup-status">Recovery phrase imported</div>
            </template>
          </div>
        </div>
      </div>
      <div class="setup-actions">
        <button class="primary" type="button" :disabled="!setupCanContinue()" @click="completeSetup">Continue</button>
      </div>
    </section>
  </div>
</body>
</html>"#
);

#[cfg(test)]
mod tests {
    use super::INDEX_HTML;

    #[test]
    fn app_javascript_cachebuster_matches_package_version() {
        assert!(INDEX_HTML.contains(concat!(
            r#"<script defer src="/assets/iuna-ui.js?v="#,
            env!("CARGO_PKG_VERSION"),
            r#""></script>"#
        )));
    }

    #[test]
    fn wallet_endpoint_is_public_only_and_precedes_stratum() {
        let wallet = INDEX_HTML.find("Wallet endpoint").expect("wallet endpoint");
        let stratum = INDEX_HTML
            .find("Stratum endpoint")
            .expect("stratum endpoint");

        assert!(wallet < stratum);
        assert!(
            INDEX_HTML
                .contains(r#"<div class="panel" x-show="advancedMode() && p2pAcceptInbound">"#)
        );
        assert!(!INDEX_HTML.contains(r#"x-text="walletEndpointRestartMessage()""#));
        assert!(!INDEX_HTML.contains(r#"x-text="stratumRestartMessage()""#));
    }

    #[test]
    fn migration_panel_is_only_visible_while_legacy_utxos_remain() {
        assert!(INDEX_HTML.contains(
            r#"x-show="status.quantum_migration?.active && status.quantum_migration?.legacy_utxos > 0""#
        ));
        assert!(!INDEX_HTML.contains(
            r#"status.quantum_migration?.legacy_balance > 0 || status.quantum_migration?.hybrid_balance > 0"#
        ));
    }

    #[test]
    fn receive_panel_presents_the_current_rotating_address() {
        assert!(INDEX_HTML.contains(r#"@click="copyReceiveAddress""#));
        assert!(INDEX_HTML.contains(r#"x-text="receiveAddress()""#));
        assert!(INDEX_HTML.contains(
            "A new receive address is selected after funds are received. Previous addresses remain monitored by this wallet."
        ));
        assert!(INDEX_HTML.contains(r#"x-text="walletAddressSummary()""#));
        assert!(INDEX_HTML.contains(r#"x-show="showWalletAddresses""#));
        assert!(INDEX_HTML.contains("Funded wallet addresses"));
        assert!(!INDEX_HTML.contains("copyWalletAddress"));
        assert!(
            !INDEX_HTML
                .contains("Do not reuse this address after its key has been revealed by a spend.")
        );
    }

    #[test]
    fn burn_leader_ranks_modal_distinguishes_loading_errors_and_empty_results() {
        assert!(INDEX_HTML.contains("burnLeaderRanksLoading(selectedBurnLeaderBlock)"));
        assert!(INDEX_HTML.contains(r#"x-show="burnLeaderRanksError""#));
        assert!(
            INDEX_HTML.contains(
                "!burnLeaderRanksError && !burnLeaderRanksLoading(selectedBurnLeaderBlock)"
            )
        );
    }

    #[test]
    fn chain_rail_starts_with_the_building_block() {
        let building = INDEX_HTML
            .find("block-card block-card-building")
            .expect("building block card");
        let confirmed = INDEX_HTML
            .find(r#"<template x-for="block in blocks""#)
            .expect("confirmed block cards");

        assert!(building < confirmed);
        assert!(!INDEX_HTML.contains("Likely finalizer"));
        assert!(!INDEX_HTML.contains(">Building</div>"));
        assert!(INDEX_HTML.contains("user-select: none"));
        assert!(!INDEX_HTML.contains("syncBuildingBlockWidth()"));
        assert!(INDEX_HTML.contains("button.block-card:hover"));
        assert!(INDEX_HTML.contains("burnCountLabel(buildingBlockOverview())"));
        assert!(INDEX_HTML.contains("transferCountLabel(buildingBlockOverview())"));
        assert!(INDEX_HTML.contains("mineCountLabel(buildingBlockOverview())"));
        assert!(INDEX_HTML.contains(
            r#"<div class="block-miner" x-text="blockFinalizerLabel(buildingBlockOverview())"></div>"#
        ));
    }
}
