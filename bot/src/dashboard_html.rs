pub const HTML: &str = r##"<!DOCTYPE html>
<html lang="en">
<head>
<meta charset="utf-8">
<meta name="viewport" content="width=device-width, initial-scale=1">
<title>Polymarket Bot</title>
<style>
* { margin: 0; padding: 0; box-sizing: border-box; }
:root {
  --bg: #0a0e14;
  --panel: #12171f;
  --card: #161d27;
  --border: #1e2733;
  --text: #d0d7de;
  --muted: #6e7a88;
  --green: #2dd4a0;
  --green-dim: rgba(45,212,160,0.12);
  --red: #f0546e;
  --red-dim: rgba(240,84,110,0.12);
  --blue: #4facfe;
  --blue-dim: rgba(79,172,254,0.12);
  --yellow: #f0b429;
  --yellow-dim: rgba(240,180,41,0.12);
  --purple: #a78bfa;
  --purple-dim: rgba(167,139,250,0.12);
  --radius: 8px;
}
body {
  background: var(--bg);
  color: var(--text);
  font-family: -apple-system, 'Inter', 'Segoe UI', sans-serif;
  font-size: 13px;
  height: 100vh;
  display: flex;
  flex-direction: column;
  overflow: hidden;
}
@media (max-height: 900px) {
  .kpi-card .kpi-value { font-size: 18px; }
  .kpi-card { padding: 6px 12px; }
  .kpi-card canvas.kpi-spark { height: 18px; }
}

/* ── Top bar ── */
#topbar {
  display: flex;
  align-items: center;
  gap: 12px;
  padding: 8px 16px;
  background: var(--panel);
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
}
#topbar .logo {
  font-weight: 800;
  font-size: 14px;
  color: var(--blue);
  letter-spacing: -0.5px;
}
.badge {
  padding: 3px 10px;
  border-radius: 20px;
  font-size: 10px;
  font-weight: 700;
  letter-spacing: 0.5px;
}
.badge-dry { background: var(--yellow-dim); color: var(--yellow); }
.badge-live { background: var(--red-dim); color: var(--red); }
.badge-copy { background: var(--purple-dim); color: var(--purple); }
.spacer { flex: 1; }
.ws-indicator {
  display: flex;
  align-items: center;
  gap: 6px;
  font-size: 11px;
  color: var(--muted);
}
.ws-dot {
  width: 7px; height: 7px;
  border-radius: 50%;
}
.ws-dot.connected { background: var(--green); box-shadow: 0 0 6px var(--green); }
.ws-dot.disconnected { background: var(--red); }
@keyframes pulse { 0%,100% { opacity:1; } 50% { opacity:0.4; } }
.ws-dot.connected { animation: pulse 2s ease infinite; }

/* ── KPI Cards ── */
#kpi-strip {
  display: grid;
  grid-template-columns: repeat(5, 1fr);
  gap: 1px;
  background: var(--border);
  flex-shrink: 0;
}
.kpi-card {
  background: var(--panel);
  padding: 10px 16px;
  display: flex;
  flex-direction: column;
  gap: 2px;
}
.kpi-card .kpi-label {
  font-size: 10px;
  font-weight: 600;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.8px;
}
.kpi-card .kpi-value {
  font-size: 22px;
  font-weight: 700;
  font-family: 'SF Mono', 'Cascadia Code', monospace;
  letter-spacing: -0.5px;
}
.kpi-card .kpi-sub {
  font-size: 10px;
  color: var(--muted);
}
.kpi-card canvas.kpi-spark {
  width: 100%;
  height: 24px;
  display: block;
  margin-top: 4px;
}

/* ── Risk gauge ── */
.gauge-row {
  display: flex;
  align-items: center;
  gap: 8px;
  margin-top: 4px;
}
.gauge-track {
  flex: 1;
  height: 4px;
  background: var(--border);
  border-radius: 2px;
  overflow: hidden;
}
.gauge-fill {
  height: 100%;
  border-radius: 2px;
  transition: width 0.5s ease, background 0.3s;
}
.gauge-pct {
  font-size: 10px;
  font-family: monospace;
  color: var(--muted);
  min-width: 32px;
  text-align: right;
}

/* ── Main grid ── */
#main {
  display: grid;
  grid-template-columns: 260px 1fr 280px;
  gap: 1px;
  flex: 1;
  min-height: 0;
  background: var(--border);
  overflow: hidden;
}
.panel {
  background: var(--panel);
  display: flex;
  flex-direction: column;
  min-height: 0;
}
.panel-header {
  padding: 10px 14px;
  font-size: 11px;
  font-weight: 700;
  color: var(--muted);
  text-transform: uppercase;
  letter-spacing: 0.8px;
  border-bottom: 1px solid var(--border);
  flex-shrink: 0;
  display: flex;
  align-items: center;
  gap: 8px;
  cursor: pointer;
  user-select: none;
  transition: background 0.15s;
}
.panel-header:hover { background: rgba(79,172,254,0.04); }
.panel-header .chevron {
  margin-left: auto;
  font-size: 10px;
  color: var(--muted);
  transition: transform 0.2s ease;
  flex-shrink: 0;
}
.panel.collapsed .panel-header .chevron { transform: rotate(-90deg); }
.panel.collapsed > :not(.panel-header) { display: none !important; }
.panel-header .accent {
  width: 3px;
  height: 14px;
  border-radius: 2px;
  flex-shrink: 0;
}
.panel-header .count {
  background: var(--border);
  padding: 1px 7px;
  border-radius: 10px;
  font-size: 10px;
  font-weight: 600;
}

/* ── PnL main chart ── */
#pnl-main-canvas {
  width: 100%;
  height: 100%;
  display: block;
}

/* ── Center column ── */
#center-panel {
  display: flex;
  flex-direction: column;
  background: var(--border);
  gap: 1px;
  min-height: 0;
  overflow: hidden;
}

/* ── Leaders table ── */
#leaders-panel { flex: 0 1 auto; max-height: 30%; overflow: hidden; }
#leaders-scroll { overflow-y: auto; flex: 1; }
#leaders-table { width: 100%; border-collapse: collapse; table-layout: fixed; }
#leaders-table td:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
#leaders-table th {
  text-align: left;
  padding: 6px 14px;
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--panel);
}
#leaders-table td { padding: 6px 14px; font-size: 12px; }
#leaders-table tbody tr { transition: background 0.15s; border-bottom: 1px solid var(--border); }
#leaders-table tbody tr:hover { background: rgba(79,172,254,0.03); }
.leader-name { color: var(--text); font-weight: 600; font-size: 12px; display: block; }
.leader-addr { color: var(--muted); font-size: 10px; font-family: monospace; }
.leader-wr { text-align: right; font-family: monospace; }
.leader-positions { color: var(--muted); text-align: right; }
.leader-score { text-align: right; font-weight: 700; font-family: monospace; }
.score-top { color: var(--green); }
.score-mid { color: var(--yellow); }
.score-low { color: var(--muted); }
.score-bar-track {
  width: 40px;
  height: 3px;
  background: var(--border);
  border-radius: 2px;
  display: inline-block;
  vertical-align: middle;
  margin-left: 4px;
  overflow: hidden;
}
.score-bar-fill {
  height: 100%;
  border-radius: 2px;
  transition: width 0.5s ease;
}
.empty-state {
  padding: 28px 14px;
  text-align: center;
  color: var(--muted);
  font-size: 12px;
}

/* ── Copy targets ── */
#copy-targets-panel { flex: 1 1 0; min-height: 60px; overflow: hidden; }
#copy-targets-scroll { flex: 1; overflow-y: auto; overflow-x: hidden; }
#copy-targets-table { width: 100%; border-collapse: collapse; table-layout: fixed; }
#copy-targets-table td:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
#copy-targets-table th {
  text-align: left;
  padding: 6px 10px;
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--panel);
}
#copy-targets-table td { padding: 6px 10px; font-size: 12px; }
#copy-targets-table tbody tr { transition: background 0.15s; border-bottom: 1px solid var(--border); }
#copy-targets-table tbody tr:hover { background: rgba(79,172,254,0.03); }
.delta-pos { color: var(--green); font-weight: 600; font-family: monospace; }
.delta-neg { color: var(--red); font-weight: 600; font-family: monospace; }
.delta-zero { color: var(--muted); font-family: monospace; }
.convergence-bar {
  height: 3px;
  border-radius: 2px;
  margin-top: 3px;
  background: var(--border);
  overflow: hidden;
}
.convergence-bar .fill {
  height: 100%;
  border-radius: 2px;
  transition: width 0.5s ease;
}
.convergence-bar .fill.converged { background: var(--green); }
.convergence-bar .fill.diverged { background: var(--yellow); }
.target-price { font-family: monospace; color: var(--muted); }

/* ── Alerts ── */
#copy-events-panel { flex: 0 0 auto; max-height: 15%; overflow-y: auto; display: none; }
.copy-event {
  padding: 6px 14px;
  font-size: 12px;
  display: flex;
  align-items: center;
  gap: 8px;
  border-bottom: 1px solid var(--border);
}
.copy-event .event-badge {
  padding: 2px 8px;
  border-radius: 10px;
  font-size: 9px;
  font-weight: 700;
  flex-shrink: 0;
  letter-spacing: 0.3px;
}
.event-stop-loss .event-badge { background: var(--red-dim); color: var(--red); }
.event-price-guard .event-badge { background: var(--yellow-dim); color: var(--yellow); }
.copy-event .event-title { color: var(--text); font-weight: 500; }
.copy-event .event-detail { color: var(--muted); font-size: 11px; margin-left: auto; font-family: monospace; }

/* ── Leader trades ── */
#trade-feed-panel { flex: 0 1 auto; max-height: 25%; overflow: hidden; }
#trade-feed-scroll { overflow-y: auto; flex: 1; }
#trade-feed-table { width: 100%; border-collapse: collapse; }
#trade-feed-table th {
  text-align: left;
  padding: 6px 14px;
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--panel);
}
#trade-feed-table td { padding: 4px 14px; font-size: 12px; }
#trade-feed-table tbody tr { transition: background 0.15s; }
#trade-feed-table tbody tr:hover { background: rgba(79,172,254,0.03); }
.trade-buy { color: var(--green); font-weight: 700; }
.trade-sell { color: var(--red); font-weight: 700; }
.trade-time { color: var(--muted); font-family: monospace; font-size: 11px; }

/* ── Right panels ── */
#right-panel {
  display: flex;
  flex-direction: column;
  background: var(--border);
  gap: 1px;
  min-height: 0;
  overflow: hidden;
}

/* ── Positions ── */
#positions-panel { flex: 0 1 auto; max-height: 45%; overflow: hidden; }
#positions-scroll { flex: 1; overflow-y: auto; overflow-x: hidden; }
#positions-table { width: 100%; border-collapse: collapse; table-layout: fixed; }
#positions-table td:first-child { overflow: hidden; text-overflow: ellipsis; white-space: nowrap; }
#positions-table th {
  text-align: left;
  padding: 6px 14px;
  color: var(--muted);
  font-size: 10px;
  font-weight: 600;
  text-transform: uppercase;
  letter-spacing: 0.5px;
  border-bottom: 1px solid var(--border);
  position: sticky;
  top: 0;
  background: var(--panel);
}
#positions-table td { padding: 5px 14px; font-size: 12px; font-family: monospace; }
.pnl-pos { color: var(--green); font-weight: 600; }
.pnl-neg { color: var(--red); font-weight: 600; }

/* ── Activity log ── */
#activity-panel { flex: 1; min-height: 0; }
#activity-log { flex: 1; overflow-y: auto; padding: 2px 0; }
.activity-row {
  display: grid;
  grid-template-columns: 60px 36px 54px 50px auto;
  padding: 2px 14px;
  font-size: 11px;
  font-family: monospace;
  line-height: 1.7;
  border-bottom: 1px solid rgba(30,39,51,0.5);
}
.activity-row .time { color: var(--muted); }
.activity-row .buy { color: var(--green); font-weight: 700; }
.activity-row .sell { color: var(--red); font-weight: 700; }
.activity-row .status { color: var(--muted); }

/* ── Scrollbar ── */
::-webkit-scrollbar { width: 5px; }
::-webkit-scrollbar-track { background: transparent; }
::-webkit-scrollbar-thumb { background: var(--border); border-radius: 3px; }
::-webkit-scrollbar-thumb:hover { background: var(--muted); }

/* ── New row flash ── */
@keyframes rowFlash { from { background: rgba(79,172,254,0.08); } to { background: transparent; } }
.row-flash { animation: rowFlash 0.8s ease; }
</style>
</head>
<body>

<div id="topbar">
  <span class="logo">POLYMARKET BOT</span>
  <span id="mode" class="badge badge-dry">DRY RUN</span>
  <span id="copy-badge" class="badge badge-copy">COPY</span>
  <span class="spacer"></span>
  <span class="ws-indicator">
    <span id="ws-status" class="ws-dot disconnected"></span>
    <span id="ws-label">disconnected</span>
  </span>
</div>

<!-- KPI Cards -->
<div id="kpi-strip">
  <div class="kpi-card">
    <span class="kpi-label">Total PnL</span>
    <span class="kpi-value" id="kpi-pnl" style="color:var(--green)">$0.00</span>
    <canvas class="kpi-spark" id="pnl-spark"></canvas>
  </div>
  <div class="kpi-card">
    <span class="kpi-label">Exposure</span>
    <span class="kpi-value" id="kpi-exposure">$0.00</span>
    <div class="gauge-row">
      <div class="gauge-track"><div class="gauge-fill" id="exposure-gauge" style="width:0%;background:var(--blue)"></div></div>
      <span class="gauge-pct" id="exposure-pct">0%</span>
    </div>
    <span class="kpi-sub" id="exposure-limit">of $200.00 limit</span>
  </div>
  <div class="kpi-card">
    <span class="kpi-label">Loss Budget</span>
    <span class="kpi-value" id="kpi-loss-budget" style="color:var(--green)">$20.00</span>
    <div class="gauge-row">
      <div class="gauge-track"><div class="gauge-fill" id="loss-gauge" style="width:0%;background:var(--green)"></div></div>
      <span class="gauge-pct" id="loss-pct">0%</span>
    </div>
    <span class="kpi-sub" id="loss-limit">of $20.00 limit</span>
  </div>
  <div class="kpi-card">
    <span class="kpi-label">Leaders</span>
    <span class="kpi-value" id="kpi-leaders" style="color:var(--purple)">0</span>
    <span class="kpi-sub" id="kpi-leaders-sub">discovering...</span>
  </div>
  <div class="kpi-card">
    <span class="kpi-label">Tracking</span>
    <span class="kpi-value" id="kpi-tracking" style="color:var(--yellow)">0</span>
    <span class="kpi-sub">copy targets</span>
  </div>
</div>

<div id="main">
  <!-- Left: PnL Chart -->
  <div id="pnl-panel" class="panel">
    <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--green)"></span> PnL<span class="chevron">&#9660;</span></div>
    <div id="pnl-main-chart" style="flex:1;padding:12px;">
      <canvas id="pnl-main-canvas"></canvas>
    </div>
  </div>

  <!-- Center: Copy Trading -->
  <div id="center-panel">
    <div id="leaders-panel" class="panel">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--purple)"></span> Leaders <span class="count" id="leader-count-badge">0</span><span class="chevron">&#9660;</span></div>
      <div id="leaders-scroll">
        <table id="leaders-table">
          <thead><tr>
            <th>Leader</th>
            <th>PnL</th>
            <th style="text-align:right">Win%</th>
            <th style="text-align:right">Score</th>
            <th style="text-align:right">Pos</th>
          </tr></thead>
          <tbody id="leaders-body">
            <tr><td colspan="5" class="empty-state">Discovering leaders...</td></tr>
          </tbody>
        </table>
      </div>
    </div>
    <div id="copy-events-panel" class="panel" style="display:none">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--red)"></span> Alerts<span class="chevron">&#9660;</span></div>
      <div id="copy-events"></div>
    </div>
    <div id="copy-targets-panel" class="panel">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--yellow)"></span> Copy Targets <span class="count" id="target-count-badge">0</span><span class="chevron">&#9660;</span></div>
      <div id="copy-targets-scroll">
        <table id="copy-targets-table">
          <thead><tr>
            <th>Market</th>
            <th style="text-align:right">Leaders</th>
            <th style="text-align:right">Resolves</th>
            <th style="text-align:right">Target</th>
            <th style="text-align:right">Ours</th>
            <th style="text-align:right">Delta</th>
            <th style="text-align:right">Price</th>
          </tr></thead>
          <tbody id="copy-targets-body">
            <tr><td colspan="7" class="empty-state">Waiting for leader data...</td></tr>
          </tbody>
        </table>
      </div>
    </div>
    <div id="trade-feed-panel" class="panel">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--green)"></span> Leader Trades<span class="chevron">&#9660;</span></div>
      <div id="trade-feed-scroll">
        <table id="trade-feed-table">
          <thead><tr>
            <th>Time</th>
            <th>Leader</th>
            <th>Side</th>
            <th>Market</th>
            <th style="text-align:right">Size</th>
            <th style="text-align:right">Price</th>
          </tr></thead>
          <tbody id="trade-feed-body">
            <tr><td colspan="6" class="empty-state">Waiting for trades...</td></tr>
          </tbody>
        </table>
      </div>
    </div>
  </div>

  <!-- Right: Positions + Activity -->
  <div id="right-panel">
    <div id="positions-panel" class="panel">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--green)"></span> Positions<span class="chevron">&#9660;</span></div>
      <div id="positions-scroll">
        <table id="positions-table">
          <thead><tr>
            <th>Market</th>
            <th style="text-align:right">Size</th>
            <th style="text-align:right">Entry</th>
            <th style="text-align:right">PnL</th>
          </tr></thead>
          <tbody id="positions-body"></tbody>
        </table>
      </div>
    </div>
    <div id="activity-panel" class="panel">
      <div class="panel-header" onclick="togglePanel(this)"><span class="accent" style="background:var(--text)"></span> Activity<span class="chevron">&#9660;</span></div>
      <div id="activity-log"></div>
    </div>
  </div>
</div>

<script>
// ── State ──
var pnlHistory = [];
var exposureHistory = [];
var tokenTitles = {};
var maxExposure = 200;
var dailyLossLimit = 20;
var MAX_PNL = 600;
var MAX_LOG = 200;

// ── DOM refs ──
var $mode = document.getElementById('mode');
var $wsDot = document.getElementById('ws-status');
var $wsLabel = document.getElementById('ws-label');
var $kpiPnl = document.getElementById('kpi-pnl');
var $kpiExposure = document.getElementById('kpi-exposure');
var $kpiLossBudget = document.getElementById('kpi-loss-budget');
var $kpiLeaders = document.getElementById('kpi-leaders');
var $kpiLeadersSub = document.getElementById('kpi-leaders-sub');
var $kpiTracking = document.getElementById('kpi-tracking');
var $expGauge = document.getElementById('exposure-gauge');
var $expPct = document.getElementById('exposure-pct');
var $expLimit = document.getElementById('exposure-limit');
var $lossGauge = document.getElementById('loss-gauge');
var $lossPct = document.getElementById('loss-pct');
var $lossLimit = document.getElementById('loss-limit');
var $pnlSpark = document.getElementById('pnl-spark');
var $pnlMainCanvas = document.getElementById('pnl-main-canvas');
var $posBody = document.getElementById('positions-body');
var $actLog = document.getElementById('activity-log');
var $leadBody = document.getElementById('leaders-body');
var $leadCountBadge = document.getElementById('leader-count-badge');
var $targetCountBadge = document.getElementById('target-count-badge');
var $copyBody = document.getElementById('copy-targets-body');
var $tradeFeed = document.getElementById('trade-feed-body');
var $copyEvents = document.getElementById('copy-events');
var $copyEventsPanel = document.getElementById('copy-events-panel');

// ── Helpers ──
function shortId(id) {
  return id.length > 16 ? id.slice(0, 6) + '\u2026' + id.slice(-4) : id;
}
function shortAddr(addr) {
  if (addr.length >= 42) return addr.slice(0, 6) + '\u2026' + addr.slice(-4);
  return addr;
}
function now() {
  return new Date().toLocaleTimeString('en-US', {hour12:false, hour:'2-digit', minute:'2-digit', second:'2-digit'});
}
function fmtDollar(v) {
  if (v === undefined || v === null) return '$0.00';
  var n = parseFloat(v);
  if (isNaN(n)) return '$0.00';
  var sign = n < 0 ? '-' : '';
  return sign + '$' + Math.abs(n).toFixed(2);
}

// ── Color helper: hex or rgb() -> rgba string ──
function toRgba(color, alpha) {
  if (color.charAt(0) === '#') {
    var hex = color.slice(1);
    if (hex.length === 3) hex = hex[0]+hex[0]+hex[1]+hex[1]+hex[2]+hex[2];
    var r = parseInt(hex.slice(0,2), 16);
    var g = parseInt(hex.slice(2,4), 16);
    var b = parseInt(hex.slice(4,6), 16);
    return 'rgba(' + r + ',' + g + ',' + b + ',' + alpha + ')';
  }
  // Handle rgb(r,g,b) format
  var m = color.match(/rgb\(\s*(\d+)\s*,\s*(\d+)\s*,\s*(\d+)\s*\)/);
  if (m) return 'rgba(' + m[1] + ',' + m[2] + ',' + m[3] + ',' + alpha + ')';
  return color;
}

// ── HTML escaping for XSS prevention ──
function escapeHtml(s) {
  if (s === undefined || s === null) return '';
  if (typeof s !== 'string') s = String(s);
  return s.replace(/&/g,'&amp;').replace(/</g,'&lt;').replace(/>/g,'&gt;').replace(/"/g,'&quot;').replace(/'/g,'&#39;');
}

// ── Collapsible panels ──
function togglePanel(header) {
  var panel = header.parentElement;
  panel.classList.toggle('collapsed');
  var isCollapsed = panel.classList.contains('collapsed');
  // Handle copy-events-panel which uses inline display:none
  if (panel.id === 'copy-events-panel' && !isCollapsed) {
    panel.style.display = 'flex';
  }
  // Redraw PnL chart when expanding
  if (panel.id === 'pnl-panel' && !isCollapsed) {
    setTimeout(drawMainPnl, 50);
  }
  // Save state to localStorage
  var id = panel.id;
  if (id) {
    var collapsed = JSON.parse(localStorage.getItem('collapsed') || '{}');
    collapsed[id] = isCollapsed;
    localStorage.setItem('collapsed', JSON.stringify(collapsed));
  }
}
// Restore collapsed state on load
(function() {
  var collapsed = JSON.parse(localStorage.getItem('collapsed') || '{}');
  for (var id in collapsed) {
    if (collapsed[id]) {
      var el = document.getElementById(id);
      if (el) el.classList.add('collapsed');
    }
  }
})();

// ── Mini sparkline renderer ──
function drawMiniSpark(canvas, pts, color) {
  if (!pts || pts.length < 2) return;
  var ctx = canvas.getContext('2d');
  if (!ctx) return;
  var dpr = window.devicePixelRatio || 1;
  var rect = canvas.getBoundingClientRect();
  canvas.width = rect.width * dpr;
  canvas.height = rect.height * dpr;
  ctx.scale(dpr, dpr);
  var w = rect.width, h = rect.height;
  ctx.clearRect(0, 0, w, h);

  var min = pts[0], max = pts[0];
  for (var i = 1; i < pts.length; i++) {
    if (pts[i] < min) min = pts[i];
    if (pts[i] > max) max = pts[i];
  }
  var range = max - min || 0.001;

  // Area fill
  ctx.beginPath();
  for (var i = 0; i < pts.length; i++) {
    var x = (i / (pts.length - 1)) * w;
    var y = h - ((pts[i] - min) / range) * (h - 4) - 2;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.lineTo(w, h);
  ctx.lineTo(0, h);
  ctx.closePath();
  var grad = ctx.createLinearGradient(0, 0, 0, h);
  grad.addColorStop(0, toRgba(color, 0.25));
  grad.addColorStop(1, toRgba(color, 0));
  ctx.fillStyle = grad;
  ctx.fill();

  // Line
  ctx.beginPath();
  for (var i = 0; i < pts.length; i++) {
    var x = (i / (pts.length - 1)) * w;
    var y = h - ((pts[i] - min) / range) * (h - 4) - 2;
    if (i === 0) ctx.moveTo(x, y); else ctx.lineTo(x, y);
  }
  ctx.strokeStyle = color;
  ctx.lineWidth = 1.5;
  ctx.stroke();
}

// ── WebSocket ──
var _wsBackoff = 2000;
function connect() {
  var proto = location.protocol === 'https:' ? 'wss:' : 'ws:';
  var ws = new WebSocket(proto + '//' + location.host + '/ws');
  ws.onopen = function() {
    _wsBackoff = 2000; // reset on success
    $wsDot.className = 'ws-dot connected';
    $wsLabel.textContent = 'connected';
  };
  ws.onerror = function() {
    $wsDot.className = 'ws-dot disconnected';
  };
  ws.onclose = function() {
    $wsDot.className = 'ws-dot disconnected';
    $wsLabel.textContent = 'reconnecting...';
    setTimeout(connect, _wsBackoff);
    _wsBackoff = Math.min(_wsBackoff * 1.5, 30000);
  };
  ws.onmessage = function(e) {
    try {
      var msg = JSON.parse(e.data);
    } catch (err) { return; }
    switch (msg.type) {
      case 'Snapshot':       handleSnapshot(msg); break;
      case 'BookSnapshot':   handleBook(msg); break;
      case 'Trade':          handleTrade(msg); break;
      case 'OrderEvent':     handleOrder(msg); break;
      case 'PositionUpdate': handlePosition(msg); break;
      case 'TickSummary':    handleTick(msg); break;
      case 'LeaderUpdate':   handleLeaders(msg); break;
      case 'LeaderTrade':    handleLeaderTrade(msg); break;
      case 'CopyEvent':      handleCopyEvent(msg); break;
    }
  };
}

// ── Snapshot ──
function handleSnapshot(d) {
  if (d.dry_run) {
    $mode.textContent = 'DRY RUN';
    $mode.className = 'badge badge-dry';
  } else {
    $mode.textContent = 'LIVE';
    $mode.className = 'badge badge-live';
  }
  maxExposure = parseFloat(d.max_exposure) || 200;
  dailyLossLimit = parseFloat(d.daily_loss_limit) || 20;
  $expLimit.textContent = 'of ' + fmtDollar(maxExposure) + ' limit';
  $lossLimit.textContent = 'of ' + fmtDollar(dailyLossLimit) + ' limit';

  updatePnl(d.total_pnl);
  updateExposure(d.total_exposure);

  // Process leaders first so tokenTitles map is populated for positions
  if (d.leaders || d.tracked_tokens) {
    handleLeaders(d);
  }
  renderPositions(d.positions);
  d.orders.forEach(function(o) {
    addActivity(now(), o.side, o.price, o.size, 'LIVE');
  });
  drawMainPnl();
}

// ── KPI updates ──
function updatePnl(val) {
  var n = parseFloat(val);
  $kpiPnl.textContent = fmtDollar(val);
  $kpiPnl.style.color = n >= 0 ? 'var(--green)' : 'var(--red)';
  pnlHistory.push({t: Date.now(), v: n});
  if (pnlHistory.length > MAX_PNL) pnlHistory.shift();
  drawMiniSpark($pnlSpark, pnlHistory.map(function(p) { return p.v; }), n >= 0 ? '#2dd4a0' : '#f0546e');

  var used = Math.abs(Math.min(0, n));
  var remaining = Math.max(0, dailyLossLimit - used);
  $kpiLossBudget.textContent = fmtDollar(remaining);
  $kpiLossBudget.style.color = remaining > dailyLossLimit * 0.5 ? 'var(--green)' : remaining > dailyLossLimit * 0.2 ? 'var(--yellow)' : 'var(--red)';
  var lossPct = Math.min(100, (used / dailyLossLimit) * 100);
  $lossGauge.style.width = lossPct + '%';
  $lossGauge.style.background = lossPct < 50 ? 'var(--green)' : lossPct < 80 ? 'var(--yellow)' : 'var(--red)';
  $lossPct.textContent = Math.round(lossPct) + '%';
}

function updateExposure(val) {
  var n = parseFloat(val);
  $kpiExposure.textContent = fmtDollar(val);
  var pct = Math.min(100, (n / maxExposure) * 100);
  $expGauge.style.width = pct + '%';
  $expGauge.style.background = pct < 60 ? 'var(--blue)' : pct < 85 ? 'var(--yellow)' : 'var(--red)';
  $expPct.textContent = Math.round(pct) + '%';
  exposureHistory.push({t: Date.now(), v: n});
  if (exposureHistory.length > MAX_PNL) exposureHistory.shift();
}

// ── Book update (no-op in copy-trader mode) ──
function handleBook(msg) {}

function handleTrade(msg) { addActivity(now(), msg.side, msg.price, msg.size, 'FILL'); }
function handleOrder(msg) { addActivity(now(), msg.side, msg.price, '-', msg.event_type); }

function tokenLabel(id) {
  var t = tokenTitles[id];
  if (t) return t.length > 20 ? t.slice(0, 20) + '\u2026' : t;
  return shortId(id);
}

function handlePosition(msg) {
  // Find existing row by iterating (safe for any token_id characters)
  var row = null;
  var rows = $posBody.getElementsByTagName('tr');
  for (var i = 0; i < rows.length; i++) {
    if (rows[i].dataset.token === msg.token_id) { row = rows[i]; break; }
  }
  if (!row) { row = document.createElement('tr'); row.dataset.token = msg.token_id; $posBody.appendChild(row); }
  var realized = parseFloat(msg.realized_pnl) || 0;
  var unrealized = parseFloat(msg.unrealized_pnl) || 0;
  var totalPnl = realized + unrealized;
  var cls = totalPnl >= 0 ? 'pnl-pos' : 'pnl-neg';
  row.innerHTML =
    '<td title="' + escapeHtml(msg.token_id) + '">' + escapeHtml(tokenLabel(msg.token_id)) + '</td>' +
    '<td style="text-align:right">' + escapeHtml(msg.net_size) + '</td>' +
    '<td style="text-align:right">' + escapeHtml(parseFloat(msg.avg_entry_price).toFixed(4)) + '</td>' +
    '<td style="text-align:right" class="' + cls + '">' + fmtDollar(totalPnl) + '</td>';
  row.classList.add('row-flash');
  setTimeout(function() { row.classList.remove('row-flash'); }, 800);
}

function handleTick(msg) {
  updatePnl(msg.total_pnl);
  updateExposure(msg.total_exposure);
  drawMainPnl();
}

// ── Leaders ──
function handleLeaders(msg) {
  var leaders = msg.leaders || [];
  var tokens = msg.tracked_tokens || [];
  $kpiLeaders.textContent = leaders.length;
  $kpiLeadersSub.textContent = leaders.length + ' wallets';
  $kpiTracking.textContent = tokens.length;
  $leadCountBadge.textContent = leaders.length;
  $targetCountBadge.textContent = tokens.length;
  tokens.forEach(function(t) { tokenTitles[t.token_id] = t.title; });

  if (leaders.length === 0) {
    $leadBody.innerHTML = '<tr><td colspan="5" class="empty-state">Discovering leaders...</td></tr>';
  } else {
    leaders.sort(function(a, b) { return (parseFloat(b.score)||0) - (parseFloat(a.score)||0); });
    $leadBody.innerHTML = '';
    leaders.forEach(function(l) {
      var row = document.createElement('tr');
      var pnlVal = parseFloat(l.pnl);
      var cls = pnlVal >= 0 ? 'pnl-pos' : 'pnl-neg';
      var scoreVal = parseFloat(l.score);
      var scoreCls = scoreVal >= 0.6 ? 'score-top' : scoreVal >= 0.4 ? 'score-mid' : 'score-low';
      if (l.score === '-') scoreCls = 'score-low';
      var name = l.username || shortAddr(l.address);
      var barW = Math.round((scoreVal || 0) * 100);
      var barColor = scoreCls === 'score-top' ? 'var(--green)' : scoreCls === 'score-mid' ? 'var(--yellow)' : 'var(--muted)';
      row.innerHTML =
        '<td><span class="leader-name">' + escapeHtml(name) + '</span><span class="leader-addr" title="' + escapeHtml(l.address) + '">' + escapeHtml(shortAddr(l.address)) + '</span></td>' +
        '<td class="' + cls + '">' + fmtDollar(l.pnl) + '</td>' +
        '<td class="leader-wr">' + escapeHtml(l.win_rate) + '</td>' +
        '<td class="leader-score ' + scoreCls + '">' + escapeHtml(l.score) + '<div class="score-bar-track"><div class="score-bar-fill" style="width:' + barW + '%;background:' + barColor + '"></div></div></td>' +
        '<td class="leader-positions">' + l.num_positions + '</td>';
      $leadBody.appendChild(row);
    });
  }

  if (tokens.length === 0) {
    $copyBody.innerHTML = '<tr><td colspan="7" class="empty-state">No positions to track</td></tr>';
  } else {
    $copyBody.innerHTML = '';
    tokens.sort(function(a, b) { return Math.abs(parseFloat(b.delta)) - Math.abs(parseFloat(a.delta)); });
    tokens.forEach(function(t) {
      var row = document.createElement('tr');
      var d = parseFloat(t.delta);
      var target = parseFloat(t.target_size);
      var ours = parseFloat(t.our_size);
      var dcls = d > 0.01 ? 'delta-pos' : d < -0.01 ? 'delta-neg' : 'delta-zero';
      var convergence = 0;
      if (target > 0) { convergence = Math.min(100, Math.max(0, (ours / target) * 100)); }
      else if (ours === 0) { convergence = 100; }
      var barCls = convergence >= 80 ? 'converged' : 'diverged';
      var title = t.title.length > 32 ? t.title.slice(0, 32) + '\u2026' : t.title;
      var deltaSign = d > 0 ? '+' : '';
      var resolves = t.days_remaining || '-';
      var resolvesCls = '';
      if (resolves === '< 1d' || resolves === '1d') resolvesCls = 'color:var(--green);font-weight:600';
      else if (resolves === '2d') resolvesCls = 'color:var(--yellow)';
      var lc = t.leader_count || 0;
      var lcCls = lc >= 5 ? 'color:var(--green);font-weight:600' : lc >= 3 ? 'color:var(--yellow)' : 'color:var(--muted)';
      row.innerHTML =
        '<td title="' + escapeHtml(t.token_id) + '">' + escapeHtml(title) +
          '<div class="convergence-bar"><div class="fill ' + barCls + '" style="width:' + convergence + '%"></div></div></td>' +
        '<td style="text-align:right;font-family:monospace;' + lcCls + '">' + lc + '</td>' +
        '<td style="text-align:right;font-family:monospace;' + resolvesCls + '">' + escapeHtml(resolves) + '</td>' +
        '<td style="text-align:right;font-family:monospace">' + escapeHtml(t.target_size) + '</td>' +
        '<td style="text-align:right;font-family:monospace">' + escapeHtml(t.our_size) + '</td>' +
        '<td style="text-align:right" class="' + dcls + '">' + deltaSign + escapeHtml(t.delta) + '</td>' +
        '<td style="text-align:right" class="target-price">' + escapeHtml(t.leader_price) + '</td>';
      $copyBody.appendChild(row);
    });
  }
}

// ── Leader trades ──
var MAX_TRADES = 50;
var tradeCount = 0;
function handleLeaderTrade(msg) {
  if (tradeCount === 0) $tradeFeed.innerHTML = '';
  var row = document.createElement('tr');
  var sideCls = msg.side === 'BUY' ? 'trade-buy' : 'trade-sell';
  var title = msg.token_title.length > 20 ? msg.token_title.slice(0, 20) + '\u2026' : msg.token_title;
  var name = msg.leader_name.length > 10 ? msg.leader_name.slice(0, 10) + '\u2026' : msg.leader_name;
  var nameScore = parseFloat(msg.leader_score) || 0;
  var nameCls = nameScore >= 0.6 ? 'score-top' : nameScore >= 0.4 ? 'score-mid' : 'score-low';
  row.innerHTML =
    '<td class="trade-time">' + escapeHtml(msg.timestamp) + '</td>' +
    '<td class="' + nameCls + '" title="' + escapeHtml(msg.leader_address) + '">' + escapeHtml(name) + '</td>' +
    '<td class="' + sideCls + '">' + escapeHtml(msg.side) + '</td>' +
    '<td title="' + escapeHtml(msg.token_title) + '">' + escapeHtml(title) + '</td>' +
    '<td style="text-align:right;font-family:monospace">' + escapeHtml(msg.size) + '</td>' +
    '<td style="text-align:right;font-family:monospace">' + escapeHtml(msg.price) + '</td>';
  row.classList.add('row-flash');
  $tradeFeed.prepend(row);
  tradeCount++;
  while ($tradeFeed.children.length > MAX_TRADES) $tradeFeed.removeChild($tradeFeed.lastChild);
}

// ── Copy events ──
var MAX_EVENTS = 20;
function handleCopyEvent(msg) {
  if (!$copyEventsPanel.classList.contains('collapsed')) {
    $copyEventsPanel.style.display = 'flex';
  }
  var div = document.createElement('div');
  var cls = msg.event_type === 'STOP_LOSS' ? 'event-stop-loss' : 'event-price-guard';
  var label = msg.event_type === 'STOP_LOSS' ? 'STOP LOSS' : 'PRICE GUARD';
  div.className = 'copy-event ' + cls;
  var title = msg.token_title.length > 28 ? msg.token_title.slice(0, 28) + '\u2026' : msg.token_title;
  div.innerHTML =
    '<span class="event-badge">' + label + '</span>' +
    '<span class="event-title">' + escapeHtml(title) + '</span>' +
    '<span class="event-detail">' + escapeHtml(msg.details) + '</span>';
  $copyEvents.prepend(div);
  while ($copyEvents.children.length > MAX_EVENTS) $copyEvents.removeChild($copyEvents.lastChild);
}

// ── Main PnL chart (Polymarket-style) ──
var _pnlAnimId = 0;
function drawMainPnl() {
  cancelAnimationFrame(_pnlAnimId);
  _pnlAnimId = requestAnimationFrame(function() { _drawPnlFrame(); });
}
function _drawPnlFrame() {
  var canvas = $pnlMainCanvas;
  var ctx = canvas.getContext('2d');
  if (!ctx) return;
  var dpr = window.devicePixelRatio || 1;
  var rect = canvas.getBoundingClientRect();
  canvas.width = rect.width * dpr;
  canvas.height = rect.height * dpr;
  ctx.scale(dpr, dpr);
  var w = rect.width, h = rect.height;
  ctx.clearRect(0, 0, w, h);

  var pts = pnlHistory.map(function(p) { return p.v; });
  if (pts.length < 2) {
    ctx.fillStyle = '#6e7a88';
    ctx.font = '13px -apple-system, sans-serif';
    ctx.textAlign = 'center';
    ctx.fillText('Waiting for PnL data\u2026', w / 2, h / 2);
    return;
  }

  // Direction: compare first vs last value
  var firstVal = pts[0];
  var lastVal = pts[pts.length - 1];
  var isUp = lastVal >= firstVal;
  var lineColor = isUp ? '#2dd4a0' : '#f0546e';
  var fillTop = isUp ? 'rgba(45,212,160,0.25)' : 'rgba(240,84,110,0.25)';
  var fillBot = isUp ? 'rgba(45,212,160,0)' : 'rgba(240,84,110,0)';

  // Range with small padding
  var pMin = pts[0], pMax = pts[0];
  for (var i = 1; i < pts.length; i++) {
    if (pts[i] < pMin) pMin = pts[i];
    if (pts[i] > pMax) pMax = pts[i];
  }
  var pRange = pMax - pMin;
  if (pRange < 0.01) pRange = 0.01;
  var pad = pRange * 0.1;
  pMin -= pad; pMax += pad;
  pRange = pMax - pMin;

  // Layout
  var padL = 8, padR = 8, padT = 40, padB = 22;
  var cw = w - padL - padR, ch = h - padT - padB;

  function yVal(v) { return padT + ch - ((v - pMin) / pRange) * ch; }

  // Subtle horizontal grid (3 lines)
  ctx.strokeStyle = 'rgba(30,39,51,0.6)';
  ctx.lineWidth = 0.5;
  for (var g = 1; g <= 3; g++) {
    var gy = padT + (g / 4) * ch;
    ctx.beginPath(); ctx.moveTo(padL, gy); ctx.lineTo(w - padR, gy); ctx.stroke();
  }

  // Build path points
  var pathPts = [];
  for (var i = 0; i < pts.length; i++) {
    var x = padL + (i / (pts.length - 1)) * cw;
    var y = yVal(pts[i]);
    pathPts.push({ x: x, y: y });
  }

  // Gradient fill under curve
  ctx.beginPath();
  ctx.moveTo(pathPts[0].x, padT + ch);
  for (var i = 0; i < pathPts.length; i++) ctx.lineTo(pathPts[i].x, pathPts[i].y);
  ctx.lineTo(pathPts[pathPts.length - 1].x, padT + ch);
  ctx.closePath();
  var grad = ctx.createLinearGradient(0, padT, 0, padT + ch);
  grad.addColorStop(0, fillTop);
  grad.addColorStop(1, fillBot);
  ctx.fillStyle = grad;
  ctx.fill();

  // Main line
  ctx.beginPath();
  ctx.moveTo(pathPts[0].x, pathPts[0].y);
  for (var i = 1; i < pathPts.length; i++) ctx.lineTo(pathPts[i].x, pathPts[i].y);
  ctx.strokeStyle = lineColor;
  ctx.lineWidth = 2;
  ctx.lineJoin = 'round';
  ctx.stroke();

  // Current value dot + glow
  var lastPt = pathPts[pathPts.length - 1];
  ctx.beginPath();
  ctx.arc(lastPt.x, lastPt.y, 6, 0, Math.PI * 2);
  ctx.fillStyle = isUp ? 'rgba(45,212,160,0.2)' : 'rgba(240,84,110,0.2)';
  ctx.fill();
  ctx.beginPath();
  ctx.arc(lastPt.x, lastPt.y, 3.5, 0, Math.PI * 2);
  ctx.fillStyle = lineColor;
  ctx.fill();

  // Dashed line from dot to right edge
  ctx.setLineDash([3, 3]);
  ctx.strokeStyle = isUp ? 'rgba(45,212,160,0.3)' : 'rgba(240,84,110,0.3)';
  ctx.lineWidth = 1;
  ctx.beginPath();
  ctx.moveTo(lastPt.x, lastPt.y);
  ctx.lineTo(w - padR, lastPt.y);
  ctx.stroke();
  ctx.setLineDash([]);

  // Time axis
  if (pnlHistory.length >= 2) {
    var tStart = pnlHistory[0].t;
    var tEnd = pnlHistory[pnlHistory.length - 1].t;
    var tSpan = tEnd - tStart;
    if (tSpan > 0) {
      ctx.fillStyle = 'rgba(110,122,136,0.5)';
      ctx.font = '9px monospace';
      ctx.textAlign = 'center';
      var numLabels = Math.min(5, Math.floor(cw / 55));
      for (var ti = 0; ti <= numLabels; ti++) {
        var frac = ti / numLabels;
        var tx = padL + frac * cw;
        var tms = tStart + frac * tSpan;
        var dt = new Date(tms);
        var hh = ('0' + dt.getHours()).slice(-2);
        var mm = ('0' + dt.getMinutes()).slice(-2);
        ctx.fillText(hh + ':' + mm, tx, h - 4);
      }
    }
  }

  // Big PnL value top-left
  var pnlText = (lastVal >= 0 ? '+$' : '-$') + Math.abs(lastVal).toFixed(2);
  ctx.font = 'bold 20px -apple-system, sans-serif';
  ctx.textAlign = 'left';
  ctx.fillStyle = lineColor;
  ctx.fillText(pnlText, padL + 4, 24);

  // Delta indicator
  var delta = lastVal - firstVal;
  var deltaText = (delta >= 0 ? '+$' : '-$') + Math.abs(delta).toFixed(2);
  ctx.font = '11px -apple-system, sans-serif';
  ctx.fillStyle = isUp ? 'rgba(45,212,160,0.7)' : 'rgba(240,84,110,0.7)';
  var mainW = ctx.measureText(pnlText).width;
  ctx.fillText(deltaText, padL + 4 + mainW + 8, 24);
}

// ── Render positions ──
function renderPositions(positions) {
  $posBody.innerHTML = '';
  if (!positions || positions.length === 0) return;
  positions.forEach(function(p) {
    var row = document.createElement('tr');
    row.dataset.token = p.token_id;
    var realized = parseFloat(p.realized_pnl) || 0;
    var unrealized = parseFloat(p.unrealized_pnl) || 0;
    var pnl = realized + unrealized;
    var cls = pnl >= 0 ? 'pnl-pos' : 'pnl-neg';
    row.innerHTML =
      '<td title="' + escapeHtml(p.token_id) + '">' + escapeHtml(tokenLabel(p.token_id)) + '</td>' +
      '<td style="text-align:right">' + escapeHtml(p.net_size) + '</td>' +
      '<td style="text-align:right">' + escapeHtml(parseFloat(p.avg_entry_price).toFixed(4)) + '</td>' +
      '<td style="text-align:right" class="' + cls + '">' + fmtDollar(pnl) + '</td>';
    $posBody.appendChild(row);
  });
}

// ── Activity log ──
function addActivity(time, side, price, size, status) {
  var row = document.createElement('div');
  row.className = 'activity-row row-flash';
  var sc = side === 'BUY' ? 'buy' : 'sell';
  row.innerHTML =
    '<span class="time">' + escapeHtml(time) + '</span>' +
    '<span class="' + sc + '">' + escapeHtml(side) + '</span>' +
    '<span>' + escapeHtml(price) + '</span>' +
    '<span>' + escapeHtml(size) + '</span>' +
    '<span class="status">' + escapeHtml(status) + '</span>';
  $actLog.prepend(row);
  while ($actLog.children.length > MAX_LOG) $actLog.removeChild($actLog.lastChild);
}

window.addEventListener('resize', function() { drawMainPnl(); });
connect();
</script>
</body>
</html>
"##;
