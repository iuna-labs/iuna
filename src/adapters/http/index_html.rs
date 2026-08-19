pub(super) const INDEX_HTML: &str = r#"<!doctype html>
<html lang="en">
<head>
  <meta charset="utf-8">
  <meta name="viewport" content="width=device-width, initial-scale=1">
  <link rel="icon" href="data:,">
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
    .brand-mark { position: relative; width: 38px; height: 38px; display: grid; place-items: center; overflow: hidden; border: 1px solid #e8ff8d; border-radius: 8px; background: linear-gradient(145deg, #ecff8a 0%, #d5f55f 54%, #8de9cd 100%); box-shadow: inset 0 1px 0 rgba(255, 255, 255, .42), 0 10px 24px rgba(213, 245, 95, .16); user-select: none; cursor: default; }
    .brand-mark::after { content: ""; position: absolute; inset: -40% -70%; background: linear-gradient(100deg, transparent 42%, rgba(255, 255, 255, .34) 50%, transparent 58%); transform: translateX(-58%) rotate(8deg); opacity: 0; pointer-events: none; }
    .brand-mark svg { position: relative; z-index: 1; width: 24px; height: 24px; display: block; }
    .brand-mark .mark-loop { fill: none; stroke: #101315; stroke-width: 4.2; stroke-linecap: round; stroke-linejoin: round; }
    .brand-mark .mark-dot { fill: #101315; }
    .brand-mark:hover::after { animation: mark-sheen .72s ease both; }
    @keyframes mark-sheen { from { opacity: 0; transform: translateX(-58%) rotate(8deg); } 32% { opacity: 1; } to { opacity: 0; transform: translateX(58%) rotate(8deg); } }
    .side-nav { display: grid; gap: 10px; width: 100%; }
    .nav-button { width: 64px; min-height: 58px; display: grid; place-items: center; gap: 4px; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; background: transparent; color: #9fa8ad; }
    .nav-button svg { width: 21px; height: 21px; stroke: currentColor; stroke-width: 2; fill: none; }
    .nav-button svg.chain-icon { stroke-width: 1.35; }
    .nav-button span { font-size: 11px; font-weight: 800; }
    .nav-button:hover, .nav-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .settings-button { margin-top: auto; width: 64px; min-height: 54px; display: grid; place-items: center; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #9fa8ad; background: transparent; text-align: center; }
    .settings-button svg { width: 23px; height: 23px; stroke: currentColor; stroke-width: 1.9; fill: none; }
    .settings-button:hover, .settings-button.active { background: #202328; border-color: #3b4448; color: #d5f55f; }
    .version-panel { width: 64px; display: grid; gap: 4px; justify-items: center; border: 1px solid transparent; border-radius: 8px; padding: 7px 4px; color: #7f888e; background: transparent; font-size: 10px; font-weight: 850; text-align: center; }
    .version-panel.update { border-color: #566d25; color: #d5f55f; background: #1c2516; cursor: pointer; }
    .version-panel.checking { color: #a8b2b8; }
    .version-panel.failed { color: #ffb1a8; }
    .version-dot { width: 6px; height: 6px; border-radius: 999px; background: #3a4248; }
    .version-panel.update .version-dot { background: #d5f55f; box-shadow: 0 0 0 3px rgba(213, 245, 95, .12); }
    .version-panel.failed .version-dot { background: #ff8f82; }
    .version-label { line-height: 1; }
    .version-update { color: #d5f55f; font-size: 9px; line-height: 1; text-transform: uppercase; }
    .content { width: 100%; min-width: 0; overflow-x: hidden; padding: 22px 24px 48px 108px; }
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
    .table-wrap { overflow-x: auto; }
    .muted { color: #8d989f; }
    .flash { position: fixed; top: 18px; right: 18px; z-index: 80; width: min(420px, calc(100vw - 36px)); border-radius: 6px; padding: 10px 12px; border: 1px solid; font-weight: 700; box-shadow: 0 18px 48px rgba(0, 0, 0, .38); }
    .flash.success { color: #d5f55f; background: #1c2516; border-color: #566d25; }
    .flash.error { color: #ffb1a8; background: #2a1717; border-color: #713434; }
    .persistent-banner { border: 1px solid #566d25; border-radius: 8px; padding: 10px 12px; margin: -4px 0 16px; color: #d5f55f; background: #1c2516; font-weight: 800; }
    .ok { color: #d5f55f; }
    .page-title { margin-bottom: 16px; }
    .setup-overlay { position: fixed; inset: 0; z-index: 30; display: grid; place-items: center; padding: 22px; background: rgba(8, 9, 10, .72); backdrop-filter: blur(8px); }
    .transaction-overlay { z-index: 40; }
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
    .metrics-head { display: flex; justify-content: space-between; gap: 12px; align-items: center; }
    .metrics-head h2 { margin: 0; }
    .metrics-range { flex: 0 0 auto; }
    .metrics-range button { padding: 5px 9px; font-size: 12px; white-space: nowrap; }
    .metrics-subhead { display: flex; justify-content: space-between; gap: 12px; align-items: baseline; margin-top: 8px; }
    .metrics-subhead h2 { margin: 0; }
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
    .leaderboard-grid { display: grid; grid-template-columns: repeat(auto-fit, minmax(min(100%, 300px), 1fr)); gap: 12px; }
    .leaderboard-card { min-width: 0; border: 1px solid #2a3035; border-radius: 8px; padding: 12px; background: #181b1f; }
    .leaderboard-card h3 { margin: 0 0 10px; }
    .leaderboard-list { display: grid; gap: 8px; }
    .leaderboard-row { display: grid; grid-template-columns: 54px minmax(0, 1fr) auto; gap: 10px; align-items: center; border: 1px solid #2f363c; border-radius: 8px; padding: 9px; background: #111316; }
    .leaderboard-rank { display: grid; place-items: center; min-height: 30px; border: 1px solid #3a4248; border-radius: 999px; color: #a8b2b8; font-size: 11px; font-weight: 900; }
    .leaderboard-rank.medal-1 { border-color: #ffd070; background: #2d2513; color: #ffd070; }
    .leaderboard-rank.medal-2 { border-color: #c7d0d5; background: #20252a; color: #e8edf0; }
    .leaderboard-rank.medal-3 { border-color: #c69262; background: #2a1f17; color: #ffc18a; }
    .leaderboard-main { min-width: 0; display: grid; gap: 3px; }
    .leaderboard-amount { color: #d5f55f; font-size: 13px; font-weight: 900; font-variant-numeric: tabular-nums; white-space: nowrap; }
    .wallet-grid { width: 100%; display: grid; grid-template-columns: minmax(0, 1fr) minmax(300px, .8fr); gap: 12px; align-items: start; }
    .wallet-actions { display: grid; gap: 12px; }
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
    .peer-remove { padding: 4px 7px; border-color: #4f3737; background: #221717; color: #ffb1a8; font-size: 12px; }
    .peer-remove:hover { border-color: #ffb1a8; color: #ffd4cf; }
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
    .block-card:hover { border-color: #d5f55f; color: #d5f55f; }
    .block-card.selected { background: #202616; border-color: #d5f55f; box-shadow: inset 0 0 0 1px #d5f55f; }
    .block-card.new-block { animation: block-arrive .45s ease both; }
    @keyframes block-arrive { from { opacity: .2; transform: translateX(-12px); } to { opacity: 1; transform: translateX(0); } }
    .block-height { font-size: 18px; font-weight: 900; }
    .block-meta { display: flex; gap: 8px; color: #8d989f; font-size: 12px; }
    .block-miner { font-family: ui-monospace, SFMono-Regular, Menlo, Consolas, monospace; font-size: 11px; overflow-wrap: anywhere; color: #9eb3bc; }
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
    .detail-link code { color: inherit; text-decoration: underline; text-underline-offset: 3px; }
    .fee-penalty-value.penalty { color: #ffb1a8; font-weight: 900; }
    .rank-list { display: grid; gap: 8px; }
    .rank-row { display: grid; grid-template-columns: 52px minmax(0, 1fr); gap: 10px; align-items: start; border: 1px solid #30383d; border-radius: 8px; padding: 10px; background: #15191d; }
    .rank-number { color: #d7f2ff; font-weight: 700; }
    .rank-details { display: grid; gap: 6px; min-width: 0; }
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
      .app-shell { display: grid; grid-template-columns: 1fr; }
      .sidebar { position: sticky; inset: auto; width: auto; height: auto; flex-direction: row; justify-content: space-between; padding: 8px; border-right: 0; border-bottom: 1px solid #262b2f; }
      .brand-mark { width: 34px; height: 34px; }
      .brand-mark svg { width: 22px; height: 22px; }
      .side-nav { display: flex; width: auto; gap: 8px; }
      .nav-button { width: 52px; min-height: 48px; }
      .nav-button span { font-size: 10px; }
      .settings-button, .version-panel { margin-top: 0; width: 48px; min-height: 48px; padding: 6px 3px; }
      .settings-button svg { width: 21px; height: 21px; }
      .content { padding: 16px 12px 36px; }
      header, .split, .setup-grid, .wallet-grid, .mining-grid, .detail-grid, .wallet-tx-row { grid-template-columns: 1fr; }
      header { display: grid; }
      .settings-mode-row { align-items: flex-start; }
      .metrics-head { align-items: flex-start; flex-direction: column; }
      .metrics-range { width: 100%; }
      .metrics-range button { flex: 1 1 0; }
      .segmented.setup-mode-picker { grid-template-columns: 1fr; }
      .metrics-grid { grid-template-columns: 1fr; }
      input { min-width: 0; width: 100%; }
      .switch input { width: auto; }
      .seed-grid { grid-template-columns: repeat(2, minmax(0, 1fr)); }
      .block-card { flex-basis: 108px; }
    }
  </style>
  <script defer src="/assets/iuna-ui.js?v=109"></script>
  <script defer src="/assets/alpine.min.js"></script>
</head>
<body x-data="iunaApp()" x-init="init()" @keydown.window.escape="closeModals()" x-cloak>
  <div class="app-shell">
    <aside class="sidebar" aria-label="iuna navigation">
      <div class="brand-mark" title="iuna" aria-label="iuna"><svg viewBox="0 0 32 32" aria-hidden="true" focusable="false"><circle class="mark-dot" cx="9.4" cy="7.6" r="2.8"></circle><path class="mark-loop" d="M9.4 13v7.1c0 3.7 2.9 6.4 6.6 6.4s6.6-2.7 6.6-6.4V13"></path></svg></div>
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
      </nav>
      <button class="settings-button" :class="{ active: tab === 'settings' }" type="button" @click="setTab('settings')" title="Settings" aria-label="Settings">
        <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M9.7 3.2 9.2 5.5a7.2 7.2 0 0 0-1.4.8L5.6 5.6 3.2 9.8l1.7 1.6a7.8 7.8 0 0 0 0 1.6l-1.7 1.6 2.4 4.2 2.2-.7a7.2 7.2 0 0 0 1.4.8l.5 2.3h4.8l.5-2.3a7.2 7.2 0 0 0 1.4-.8l2.2.7 2.4-4.2-1.7-1.6a7.8 7.8 0 0 0 0-1.6L21 9.8l-2.4-4.2-2.2.7a7.2 7.2 0 0 0-1.4-.8l-.5-2.3H9.7Z"></path><circle cx="12" cy="12.2" r="3.1"></circle></svg>
      </button>
      <button class="version-panel" type="button" :class="{ update: updateAvailable(), checking: releaseCheckState === 'checking', failed: releaseCheckState === 'failed' }" :title="versionPanelTitle()" @click="openLatestRelease">
        <span class="version-dot" aria-hidden="true"></span>
        <span class="version-label" x-text="appVersionLabel()"></span>
        <span class="version-update" x-show="updateAvailable()">Update</span>
      </button>
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

    <section x-show="tab === 'wallet'">
      <div class="page-title">
        <button class="wallet-balance-line" type="button" @click="openWalletUtxosModal" title="Show wallet UTXOs">
          <span class="tx-label">Balance</span>
          <span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span>
        </button>
      </div>
      <div class="wallet-grid">
        <div class="wallet-actions">
          <div class="panel">
            <h3>Send</h3>
            <form @submit.prevent="sendTransfer">
              <div class="recipient-field">
                <label>Recipient<input x-model="transferTo" @input="scheduleFeeEstimates" autocomplete="off" required></label>
                <button class="icon-button" type="button" @click="openAddressBookPicker()" title="Choose contact" aria-label="Choose contact">
                  <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 5.5A2.5 2.5 0 0 1 6.5 3H20v18H6.5A2.5 2.5 0 0 1 4 18.5z"></path><path d="M8 7h8"></path><path d="M8 11h6"></path><path d="M8 15h4"></path></svg>
                </button>
              </div>
              <div class="amount-field">
                <label>Amount<input x-model="transferAmount" @input="scheduleFeeEstimates" type="number" min="0.000001" step="0.000001" required></label>
                <button class="amount-max-button" type="button" @click="setMaxTransferAmount" :disabled="transferMaxDisabled()" title="Use maximum spendable amount">Max</button>
              </div>
              <label>Fee / byte<input x-model="transferFee" @input="scheduleFeeEstimates" type="number" min="0" step="0.000001" required></label>
              <div class="fee-preview" x-text="feeEstimateLabel('transfer')"></div>
              <button class="advanced-toggle" type="button" @click="toggleSendAdvanced" x-text="showSendAdvanced ? 'Hide UTXOs' : 'UTXOs'"></button>
              <div class="send-utxo-summary" x-show="showSendAdvanced">
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
              <button class="primary" type="submit">Send</button>
            </form>
          </div>
          <div class="panel">
            <div class="panel-head">
              <h3>Receive</h3>
              <button type="button" @click="copyAddress">Copy</button>
            </div>
            <div class="receive-address">
              <div class="muted">Public key / address</div>
              <div class="address-box"><code x-text="status.wallet_address || '-'"></code></div>
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
                  <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="shortAddressLabel(tx.from)"></code></div>
                  <div class="tx-field" x-show="tx.to"><span class="tx-label">To</span><code class="tx-value hash" x-text="shortAddressLabel(tx.to)"></code></div>
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
              <code class="mine-stat-value" x-text="currentFinalizerLabel()"></code>
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
          <div class="panel-separator"></div>
          <div class="stratum-config">
            <div class="stratum-note">Start the node with <code>--stratum 0.0.0.0:3333</code> to expose a Stratum V1 endpoint for ASIC miners. Use the pool URL below in the miner configuration.</div>
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
            <div class="network-health-detail" x-text="networkHealth.last_error || 'No peer errors reported'"></div>
          </div>
          <div class="network-health-grid">
            <div class="peer-summary-item"><div class="peer-summary-label">Local Height</div><div class="peer-summary-value" x-text="networkHealth.local_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Best Known</div><div class="peer-summary-value" x-text="networkHealth.best_known_height ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Lag</div><div class="peer-summary-value" x-text="networkLagLabel()"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Stale</div><div class="peer-summary-value" x-text="networkHealth.stale_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Banned</div><div class="peer-summary-value" x-text="networkHealth.banned_peers ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Mempool</div><div class="peer-summary-value" x-text="networkHealth.pending_transactions ?? '-'"></div></div>
            <div class="peer-summary-item"><div class="peer-summary-label">Plain Tx</div><div class="peer-summary-value" x-text="networkHealth.pending_plain_transactions ?? '-'"></div></div>
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
          <table>
            <thead><tr><th>Status</th><th>Address</th><th>Direction</th><th>Last Contact</th><th x-show="developmentMode()">Clock</th><th x-show="developmentMode()">Ban</th><th x-show="developmentMode()">Score</th><th>Height</th><th x-show="developmentMode()">Delta</th><th x-show="developmentMode()">Tip</th><th x-show="developmentMode()">Sent</th><th x-show="developmentMode()">Received</th><th x-show="developmentMode()">Last Error</th><th>Actions</th></tr></thead>
            <tbody>
              <template x-for="peer in peers" :key="peer.address">
                <tr>
                  <td><span class="peer-status" :class="peerStatus(peer)" x-text="peerStatusLabel(peer)"></span></td>
                  <td><code x-text="peer.address"></code></td>
                  <td x-text="peer.direction"></td>
                  <td x-text="peerLastContactLabel(peer)"></td>
                  <td x-show="developmentMode()" x-text="peerClockLabel(peer)"></td>
                  <td x-show="developmentMode()" x-text="peerBanLabel(peer)"></td>
                  <td x-show="developmentMode()" x-text="peer.misbehavior_score ?? 0"></td>
                  <td x-text="peer.last_known_height ?? '-'"></td>
                  <td x-show="developmentMode()" x-text="peerHeightDelta(peer)"></td>
                  <td x-show="developmentMode()"><code x-text="short(peer.last_known_tip_hash)"></code></td>
                  <td x-show="developmentMode()" x-text="peer.messages_sent"></td>
                  <td x-show="developmentMode()" x-text="peer.messages_received"></td>
                  <td x-show="developmentMode()" x-text="peer.last_error || ''"></td>
                  <td><div class="peer-actions"><button class="peer-remove" type="button" x-show="canRemovePeer(peer)" @click="removePeer(peer)">Remove</button><span class="muted" x-show="!canRemovePeer(peer)">Observed</span></div></td>
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
            <template x-for="block in blocks" :key="block.hash">
              <button class="block-card" :class="{ selected: selectedBlock?.hash === block.hash, 'new-block': newBlockHashes.has(block.hash) }" @click="selectBlock(block)" type="button">
                <div class="block-height" x-text="block.height"></div>
                <div class="block-meta">
                  <span x-text="burnCountLabel(block)"></span>
                  <span x-text="transferCountLabel(block)"></span>
                  <span x-text="mineCountLabel(block)"></span>
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
                <div class="detail-kv"><div class="key">Mode</div><div x-text="selectedBlock.finalizer_mode === 'recovery' ? 'Recovery' : `Rank ${selectedBlock.finalizer_rank ?? 0}`"></div></div>
                <div class="detail-kv"><div class="key">Reward</div><div>IUNA <span x-text="amountLabel(selectedBlock.reward)"></span></div></div>
                <div class="detail-kv"><div class="key">Burn Bundles</div><div x-text="blockBurnBundleRatio(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Burns</div><div x-text="blockBurnCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Transfers</div><div x-text="blockTransferCount(selectedBlock)"></div></div>
                <div class="detail-kv"><div class="key">Total Burned</div><div>IUNA <span x-text="amountLabel(blockBurned(selectedBlock))"></span></div></div>
                <div class="detail-kv">
                  <div class="key">Bytes</div>
                  <button class="detail-link" type="button" @click="openBlockBytesModal(selectedBlock)" title="Block byte breakdown">
                    <span x-text="blockTotalBytes(selectedBlock)"></span>B
                  </button>
                </div>
                <div class="detail-kv"><div class="key">VDF</div><div><span x-text="selectedBlock.vdf_rounds"></span> rounds</div></div>
              </div>
              <div class="tx-list">
                <h3>Transactions</h3>
                <div class="tx-section">
                  <div class="tx-section-title"><span>Envelope</span><span class="tx-section-meta" x-text="shortAddressLabel(selectedBlock.miner)"></span></div>
                  <div class="tx-scroll-list">
                    <template x-for="tx in selectedBlock.transactions" :key="tx.signature">
                      <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Envelope', blockHeight: selectedBlock.height, blockFinalizer: selectedBlock.miner })">
                        <span class="pill" :class="txPillClass(tx)" x-text="txPillLabel(tx)"></span>
                        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="shortAddressLabel(txFrom(tx))"></code></div>
                        <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="shortAddressLabel(txTo(tx))"></code></div>
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
                    <summary class="tx-section-title"><span x-text="`Burn bundle ${bundle.slot}`"></span><span class="tx-section-meta"><span x-text="shortAddressLabel(bundle.member)"></span> · <span x-text="bundle.byte_size || bundle.byteSize || 0"></span>B</span></summary>
                    <template x-for="tx in bundle.burns" :key="tx.signature">
                      <div class="tx-card" role="button" tabindex="0" @click="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })" @keydown.enter.prevent="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })" @keydown.space.prevent="openTransactionModal(tx, { source: 'Burn bundle', blockHeight: selectedBlock.height, blockFinalizer: bundle.member })">
                        <span class="pill" :class="txPillClass(tx)" x-text="txPillLabel(tx)"></span>
                        <div class="tx-field"><span class="tx-label">Amount</span><span class="tx-value money">IUNA <span x-text="amountLabel(txAmount(tx))"></span></span></div>
                        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(tx)"></span></div>
                        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="shortAddressLabel(txFrom(tx))"></code></div>
                        <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="shortAddressLabel(txTo(tx))"></code></div>
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
                <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="shortAddressLabel(txFrom(tx))"></code></div>
                <div class="tx-field" x-show="txTo(tx)"><span class="tx-label">To</span><code class="tx-value hash" x-text="shortAddressLabel(txTo(tx))"></code></div>
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
          <h2>Metrics</h2>
          <div class="segmented metrics-range" role="group" aria-label="Metrics block range">
            <button type="button" :class="{ active: metricsRange === 100 }" @click="setMetricsRange(100)">Last 100</button>
            <button type="button" :class="{ active: metricsRange === 1000 }" @click="setMetricsRange(1000)">Last 1000</button>
            <button type="button" :class="{ active: metricsRange === 'all' }" @click="setMetricsRange('all')">All</button>
          </div>
        </div>
        <div class="metrics-summary">
          <div class="metric"><div class="label">Latest block</div><div class="value" x-text="metricsLatest().height ?? '-'"></div></div>
          <div class="metric"><div class="label">Supply</div><div class="value" x-text="metricAmountLabel(metricsLatest().circulatingSupply)"></div></div>
          <div class="metric"><div class="label">Known addresses</div><div class="value" x-text="metricsLatest().knownWalletAddresses ?? '-'"></div></div>
          <div class="metric"><div class="label">Total burned</div><div class="value" x-text="metricAmountLabel(metricsLatest().totalBurnedAmount)"></div></div>
          <div class="metric"><div class="label">Difficulty</div><div class="value" x-text="metricsLatest().mineDifficultyBits ?? '-'"></div></div>
        </div>
        <div class="metrics-grid" x-show="loadingMetrics && metricsCharts().length === 0">
          <article class="metric-chart-card skeleton-card" aria-hidden="true">
            <div class="metric-chart-head"><div class="skeleton-line medium"></div><div class="skeleton-line short"></div></div>
            <div class="metric-chart-frame"><div class="skeleton-line long"></div></div>
          </article>
          <article class="metric-chart-card skeleton-card" aria-hidden="true">
            <div class="metric-chart-head"><div class="skeleton-line short"></div><div class="skeleton-line medium"></div></div>
            <div class="metric-chart-frame"><div class="skeleton-line long"></div></div>
          </article>
        </div>
        <div class="metrics-empty" x-show="metricsCharts().length === 0 && !loadingMetrics">No metrics collected yet</div>
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
        <div class="metrics-subhead">
          <h2>Leaderboards</h2>
        </div>
        <div class="leaderboard-grid">
          <template x-for="board in [{ key: 'balances', title: 'Top 10 Balance' }, { key: 'miners', title: 'Top 10 Miners' }, { key: 'burners', title: 'Top 10 Burners' }]" :key="board.key">
            <article class="leaderboard-card">
              <h3 x-text="board.title"></h3>
              <div class="leaderboard-list">
                <template x-for="(row, index) in leaderboardRows(board.key)" :key="`${board.key}-${row.address}`">
                  <div class="leaderboard-row">
                    <div class="leaderboard-rank" :class="leaderboardRankClass(index)" x-text="leaderboardRankLabel(index)"></div>
                    <div class="leaderboard-main">
                      <code class="tx-value hash" x-text="shortAddressLabel(row.address)"></code>
                      <div class="muted" x-text="leaderboardCountLabel(board.key, row)"></div>
                    </div>
                    <div class="leaderboard-amount" x-text="leaderboardAmountLabel(row)"></div>
                  </div>
                </template>
                <div class="metrics-empty" x-show="leaderboardRows(board.key).length === 0">No entries</div>
              </div>
            </article>
          </template>
        </div>
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
        <div class="panel">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Development mode</div>
              <div class="muted" x-text="developmentMode() ? 'Detailed P2P data and the metrics screen are available.' : 'P2P stays focused on a simple peer list.'"></div>
            </div>
            <label class="toggle-switch" :class="{ active: keepTrackOfMetrics }">
              <input type="checkbox" :checked="keepTrackOfMetrics" @change="setKeepTrackOfMetrics($event.target.checked)">
              <span class="toggle-track" aria-hidden="true"><span class="toggle-thumb"></span></span>
              <span class="toggle-text" x-text="developmentMode() ? 'On' : 'Off'"></span>
            </label>
          </div>
        </div>
        <div class="panel" x-show="advancedMode()">
          <div class="settings-mode-row">
            <div class="settings-mode-copy">
              <div class="settings-mode-title">Recovery VDF</div>
              <div class="muted">Top <span x-text="recoveryVdfTopRankPercent"></span>% threshold for fallback/recovery work.</div>
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
  <div class="setup-overlay transaction-overlay" x-show="showWalletUtxos" x-transition.opacity @click.self="closeWalletUtxosModal()" role="dialog" aria-modal="true" aria-labelledby="wallet-utxos-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="wallet-utxos-title">Wallet UTXOs</h2>
          <div class="tx-field"><span class="tx-label">Total</span><span class="tx-value money">IUNA <span x-text="amountLabel(status.wallet_balance)"></span></span></div>
        </div>
        <button type="button" @click="closeWalletUtxosModal">Close</button>
      </div>
      <div class="utxo-list">
        <template x-for="utxo in walletUtxos" :key="`${utxo.outpoint.txid}:${utxo.outpoint.index}`">
          <div class="wallet-utxo-row">
            <div class="utxo-node-label"><span>UTXO</span><span class="utxo-node-amount">IUNA <span x-text="amountLabel(utxo.amount)"></span></span></div>
            <div class="tx-field"><span class="tx-label">Outpoint</span><code class="tx-value hash" x-text="txInputOutpoint({ outpoint: utxo.outpoint })"></code></div>
            <div class="tx-field"><span class="tx-label">Address</span><code class="tx-value hash" x-text="addressLabel(utxo.address)"></code></div>
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
          <h2 id="block-bytes-title" x-text="selectedByteBlock ? `Block ${selectedByteBlock.height} Bytes` : 'Block Bytes'"></h2>
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
  <div class="setup-overlay transaction-overlay" x-show="selectedBurnLeaderBlock" x-transition.opacity @click.self="closeBurnLeaderRanksModal()" role="dialog" aria-modal="true" aria-labelledby="burn-ranks-title">
    <section class="tx-modal">
      <div class="tx-modal-head">
        <div class="tx-modal-title">
          <h2 id="burn-ranks-title" x-text="burnLeaderRanksTitle(selectedBurnLeaderBlock)"></h2>
          <div class="tx-field"><span class="tx-label">Finalizer</span><code class="tx-value hash" x-text="selectedBurnLeaderBlock ? addressLabel(selectedBurnLeaderBlock.miner) : '-'"></code></div>
        </div>
        <button type="button" @click="closeBurnLeaderRanksModal">Close</button>
      </div>
      <div class="rank-list">
        <template x-for="rank in burnLeaderRanks(selectedBurnLeaderBlock)" :key="`${selectedBurnLeaderBlock.hash}-${rank.rank}-${rank.ticket_id ?? rank.ticketId}`">
          <div class="rank-row">
            <div class="rank-number" x-text="burnLeaderRankLabel(rank)"></div>
            <div class="rank-details">
              <div class="tx-field"><span class="tx-label">Owner</span><code class="tx-value hash" x-text="addressLabel(rank.owner)"></code></div>
              <div class="tx-field"><span class="tx-label">Burn</span><span class="tx-value money">IUNA <span x-text="amountLabel(rank.amount)"></span></span></div>
              <div class="tx-field"><span class="tx-label">Ticket</span><code class="tx-value hash" x-text="short(rank.ticket_id ?? rank.ticketId)"></code></div>
              <div class="tx-field"><span class="tx-label">Eligible</span><span class="tx-value number" x-text="burnLeaderEligibilityLabel(rank)"></span></div>
            </div>
          </div>
        </template>
        <div class="tx-modal-empty" x-show="burnLeaderRanks(selectedBurnLeaderBlock).length === 0">No burn leader ranks</div>
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
        <div class="tx-field"><span class="tx-label">Fee</span><span class="tx-value money" x-text="txFeeLabel(selectedTransaction?.tx)"></span></div>
        <div class="tx-field"><span class="tx-label">From</span><code class="tx-value hash" x-text="addressLabel(txFrom(selectedTransaction?.tx || {}))"></code></div>
        <div class="tx-field" x-show="txTo(selectedTransaction?.tx || {})"><span class="tx-label">To</span><code class="tx-value hash" x-text="addressLabel(txTo(selectedTransaction?.tx || {}))"></code></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Difficulty</span><span class="tx-value number" x-text="txDifficultyBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Bits</span><span class="tx-value number" x-text="txProofBits(selectedTransaction?.tx) ?? '-'"></span></div>
        <div class="tx-field" x-show="isMineTx(selectedTransaction?.tx)"><span class="tx-label">Proof Hash</span><code class="tx-value hash" x-text="txProofHash(selectedTransaction?.tx) || '-'"></code></div>
      </div>
      <div class="utxo-flow">
        <div class="utxo-column">
          <h3>Inputs</h3>
          <template x-for="(input, index) in txInputs(selectedTransaction?.tx || {})" :key="txInputKey(input, index)">
            <div class="utxo-node">
              <div class="utxo-node-label"><span>Input <span x-text="index + 1"></span></span><span>spent</span></div>
              <div class="utxo-node-ref" x-text="txInputOutpoint(input)"></div>
              <div class="tx-field"><span class="tx-label">Value</span><span class="tx-value money" x-text="txInputAmountLabel(input)"></span></div>
              <div class="tx-field"><span class="tx-label">Owner</span><code class="tx-value hash" x-text="addressLabel(input.owner)"></code></div>
              <div class="tx-field"><span class="tx-label">Sig</span><code class="tx-value hash" x-text="short(input.signature)"></code></div>
            </div>
          </template>
          <div class="tx-modal-empty" x-show="txInputs(selectedTransaction?.tx || {}).length === 0">No inputs</div>
        </div>
        <div class="utxo-arrow" aria-hidden="true">&rarr;</div>
        <div class="utxo-column">
          <h3>Outputs</h3>
          <template x-for="(output, index) in txVisualOutputs(selectedTransaction?.tx || {})" :key="txOutputKey(output, index)">
            <div class="utxo-node" :class="{ burned: output.kind === 'burned', fee: output.kind === 'fee' }">
              <div class="utxo-node-label"><span x-text="output.label"></span><span x-text="output.kind"></span></div>
              <div class="utxo-node-amount">IUNA <span x-text="amountLabel(output.amount)"></span></div>
              <template x-if="output.address">
                <div class="tx-field"><span class="tx-label">To</span><code class="tx-value hash" x-text="addressLabel(output.address)"></code></div>
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
          <span class="pill">Send</span>
          <h2 id="address-book-picker-title">Choose Contact</h2>
        </div>
        <div class="address-book-actions">
          <button class="icon-button" type="button" x-show="!addressBookModalOpen" @click="openAddressBookModal()" title="Add contact" aria-label="Add contact">
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
        <div class="setup-feedback error" x-show="addressBookDraftAddress && !validAddressBookAddress(addressBookDraftAddress)">Address must be a 64 character hex public key</div>
        <div class="address-book-modal-actions">
          <button class="icon-button modal-delete-button" type="button" x-show="addressBookEditingAddress" @click="removeAddressBookEntry({ address: addressBookEditingAddress, name: addressBookDraftName || addressBookEditingAddress })" title="Delete contact" aria-label="Delete contact">
            <svg viewBox="0 0 24 24" aria-hidden="true"><path d="M4 7h16"></path><path d="M10 11v6"></path><path d="M14 11v6"></path><path d="M6 7l1 14h10l1-14"></path><path d="M9 7V4h6v3"></path></svg>
          </button>
          <button class="icon-button" type="button" @click="closeAddressBookModal()" title="Back to contacts" aria-label="Back to contacts">
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
                <code x-text="setupAddress()"></code>
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
</html>"#;
