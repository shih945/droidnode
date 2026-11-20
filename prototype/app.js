'use strict';

/* DroidNode 控制台 · 交互原型
   数据是 2026-11-12 00:26 真机快照的整理版：节点、Pod、镜像、日志都取自
   `/pods`、`/images`、`/stats/summary` 与设备日志的实测内容。
   原型不发送任何网络请求，也不改动 APK。 */

const NODE = {
  name: 'droidnode-37e62e0fe8ce0057',
  server: 'https://192.0.2.10:6443',
  kubelet: '0.0.0.0:10255',
  ip: '192.0.2.11',
  registry: 'registry.example.com',
};

const PHASES = {
  Running: { label: '运行中', dot: 'ok' },
  Succeeded: { label: '已完成', dot: 'ok' },
  Pending: { label: '等待调度', dot: 'lamp' },
  Drained: { label: '已排空', dot: 'lamp' },
  CrashLoopBackOff: { label: '崩溃重启', dot: 'bad' },
  ImagePullBackOff: { label: '拉取失败', dot: 'bad' },
  Failed: { label: '失败', dot: 'bad' },
};

const TABS = [
  { id: 'node', label: '节点', icon: 'node', title: '节点' },
  { id: 'pods', label: 'Pod', icon: 'pod', title: 'Pod' },
  { id: 'images', label: '镜像', icon: 'image', title: '镜像' },
  { id: 'logs', label: '日志', icon: 'log', title: '日志' },
  { id: 'settings', label: '设置', icon: 'settings', title: '设置' },
];

const SHOTS = {
  node: { path: '_reference/current-ui/01-dashboard.png', caption: '当前 APK · 仪表盘 · 2026-11-12 00:26' },
  pods: { path: '_reference/current-ui/04-pods.png', caption: '当前 APK · 调试台 PODS · 同一时刻比仪表盘多一个 Pod' },
  images: { path: '_reference/current-ui/05-images.png', caption: '当前 APK · 镜像页 · 标题重复、删除按钮占满卡片' },
  logs: { path: '_reference/current-ui/02-logs.png', caption: '当前 APK · 调试台 LOGS · 时间戳与内容在同一折行里' },
  settings: { path: '_reference/current-ui/03-status.png', caption: '当前 APK · 调试台 STATUS · 没有设置页，这里是唯一的状态页' },
};

const SCREENS = {
  node: {
    title: '节点',
    purpose: '一眼判断这台手机节点还能不能接 Pod，还剩多少资源。',
    source: [
      '<code>GET /pods</code> 每 2 秒，失败时退回日志推断',
      '<code>GET /stats/summary</code> 给逐 Pod 内存（新增）',
      '<code>GET /healthz</code> 判断 agent 真的活着（新增）',
      '电池、存储、进程数走 Android 系统接口',
    ],
    actions: [
      '点节点名复制完整 ID',
      '维护模式开关 → <code>POST /drain</code> 或 <code>POST /undrain</code>',
      'Pod 行点开详情，再跳对应日志',
    ],
    diff: [
      '现状三套配色：系统 ActionBar、深色正文、浅色按钮',
      '标签靠空格对齐，中英文混排永远对不齐',
      '排空状态不可见，低电量排空后仍显示 RUNNING',
    ],
  },
  pods: {
    title: 'Pod',
    purpose: '看这一台机器上跑了什么，哪个在反复重启。',
    source: [
      '<code>GET /pods</code> 的 containerStatuses：容器名、重启次数、启动时间',
      '<code>GET /stats/summary</code>：逐容器 RSS 与 CPU',
      '镜像名沿用日志里的 <code>POD_EVENT assigned</code>',
    ],
    actions: [
      '按全部、运行中、异常过滤',
      '点行进详情：容器、镜像、IP、重启、事件',
      '详情里跳转日志，或删除 Pod',
    ],
    diff: [
      '现有 PODS 页靠日志正则，与仪表盘数据源不一致',
      '没有容器名、没有逐 Pod 资源、没有详情',
    ],
  },
  images: {
    title: '镜像',
    purpose: '知道磁盘被谁占了，删掉不再使用的镜像。',
    source: [
      '<code>GET /images</code>：层数、压缩、解包、digest、拉取时间',
      '<code>POST /images/delete</code>：409 表示镜像正在被使用',
    ],
    actions: [
      '删除前二次确认，并写明释放多少空间',
      '被 Pod 使用的镜像标记为使用中，删除按钮禁用',
      '拉取新镜像（原型用进度条模拟）',
    ],
    diff: [
      '现有删除按钮占满卡片、颜色最亮，且没有确认',
      '409 直接把 JSON 弹成 Toast',
      '没有「被哪个 Pod 使用」的信息',
    ],
  },
  logs: {
    title: '日志',
    purpose: '定位一次失败的完整过程，而不是读一整屏日志。',
    source: [
      'agent stdout 环形缓冲（500 行）',
      '<code>GET /containerLogs/{ns}/{pod}/{container}</code> 按 Pod 过滤',
    ],
    actions: [
      '级别过滤、关键字搜索、暂停、清空',
      '从 Pod 详情跳进来时只看该 Pod 的日志',
    ],
    diff: [
      '现有 LOGS 页是单一文本流，时间戳和内容混在折行里',
      '没有级别过滤、搜索、暂停，也不能按 Pod 过滤',
    ],
  },
  settings: {
    title: '设置',
    purpose: '先看清这台节点现在的样子，再决定进哪一页改。',
    source: [
      '本机状态：agent IPC、节点 conditions、kubeconfig 是否存在',
      '本地配置（DataStore）与 env 文件',
      '电池优化白名单状态读系统接口',
    ],
    actions: [
      '状态摘要：节点名 / Ready、控制面地址、运行状态',
      '快捷操作：重启或停止 agent（最常用的两个）',
      '五个入口，每行的副标题就是当前值：节点与网络 / 集群连接 / 运行时与维护 / 电源与后台 / 应用与诊断',
      '页尾固定「关于」：版本、许可、源码',
    ],
    diff: [
      '原单页 9 段 30 多个控件，滚动 3–4 屏，找「停止 agent」要一路划到底',
      '一次性配置、每天操作、系统权限、界面偏好混在同一列表里，风险等级没有区分',
      '入口页看不到任何状态，必须先点进设置才知道节点连没连上',
    ],
  },
  join: {
    title: '加入集群',
    purpose: '用最小的凭据把手机接进现有集群，失败不覆盖已有配置。',
    source: [
      '<code>GET /cacerts</code> 取控制面 CA（k3s agent 用的同一入口）',
      '地址 + token：token 由 <code>kubectl create token</code> 签发',
      '文件 / 粘贴：控制面脚本生成的 kubeconfig 或管理员证书',
      '写入前先跑 TLS + 身份校验，通过才落盘并重启 agent',
    ],
    actions: [
      '方式：地址 + token（推荐）/ 导入 kubeconfig 文件 / 粘贴 kubeconfig',
      'CA 留空自动获取，并显示 SHA256 指纹',
      '401 与 403 分开提示；验证失败不覆盖现有 kubeconfig',
    ],
    diff: [
      '现有 APK 只能 adb push kubeconfig，且没有连接测试',
      '旧流程先保存再提示「连接未通过」，可能把可用节点弄断',
    ],
  },
  'settings-node': {
    title: '节点与网络',
    purpose: '这台手机以什么身份、从哪张网卡、用哪个镜像源接入集群。',
    source: ['本地配置（DataStore / env 文件）', 'NetworkInterfaces 枚举到的真实 LAN 网卡', 'files/registry-mirrors.env'],
    actions: ['改节点名、kubelet 端口', '选固定网卡或自动', '三个上游的镜像源', '日志级别', '底部唯一的提交点「保存并重启 agent」'],
    diff: ['现有 APK 把身份、端口、镜像源、日志级别平铺在同一页，且和运行控制混在一起', '改名会新建 Node 这一风险没有任何页首提示'],
  },
  'settings-cluster': {
    title: '集群连接',
    purpose: '能不能连上控制面、以什么身份连；失败时先看清楚再动。',
    source: ['本地 kubeconfig（加密）', 'GET /version 连接测试', '控制面 CA 指纹'],
    actions: ['查看地址 / 身份 / 文件', '连接测试', '加入或更换控制面（三级页）', '401 与 403 的分开解释'],
    diff: ['原设置页顶部只有一张「控制面」卡，加入入口和节点配置挤在一屏', '排障时找不到「先测连接」这条路径'],
  },
  'settings-runtime': {
    title: '运行时与维护',
    purpose: 'agent 现在活着吗、在不在接 Pod、要不要停掉它。',
    source: ['agent 状态 IPC（每秒）', '节点 conditions 与 cordon 状态'],
    actions: ['当前状态与 agent 上报（只读）', '维护模式开关', '重启 / 启动 / 停止', '停止流程说明：cordon → 停容器 → 删 Pod → 标记 NotReady'],
    diff: ['原页面把维护模式、重启、停止和电源开关放在同一张长列表里，误触代价高', '停止流程的副作用（掉线、容器停止）没有集中说明'],
  },
  'settings-power': {
    title: '电源与后台',
    purpose: '让 Android 不要把 agent 掐掉——这是唯一需要用户手动授权的部分。',
    source: ['isIgnoringBatteryOptimizations', '本地开关（自启 / 锁屏保持）', 'WakeLock / WifiLock 策略'],
    actions: ['电池优化白名单跳系统设置', '开机自启', '锁屏保持在线'],
    diff: ['原页面把权限开关和运行操作混排', 'Doze 会忽略唤醒锁这一因果没有解释，用户不知道为什么必须进白名单'],
  },
  'settings-app': {
    title: '应用与诊断',
    purpose: '界面偏好与排障导出——和节点运行无关的部分。',
    source: ['本地配置', 'LogBuffer（最近 200 行）', '构建信息'],
    actions: ['配色 / 语言', '导出诊断（系统分享）'],
    diff: ['原页面外观、语言、诊断、关于各占一段，占掉近一屏', '这些项和节点运行无关，却和危险操作同级'],
  },
};

/* ── 初始状态 ─────────────────────────────────────────────── */

const seed = Date.now();

const state = {
  screen: 'node',
  agent: 'running', // running | restarting | stopped
  startedAt: seed - 245000,
  maintained: false,
  pods: [
    { id: 'hello-android', name: 'hello-android', ns: 'default', container: 'hello', image: 'alpine:latest', phase: 'CrashLoopBackOff', ready: false, restarts: 5, startedAt: seed - 32000, memMB: 4.1, cpuPct: 0 },
    { id: 'demo-log', name: 'demo-log', ns: 'default', container: 'ticker', image: 'alpine:latest', phase: 'Running', ready: true, restarts: 0, startedAt: seed - 245000, memMB: 12.4, cpuPct: 0.6 },
    { id: 'demo-web', name: 'demo-web', ns: 'default', container: 'web', image: 'hashicorp/http-echo:1.0', phase: 'Running', ready: true, restarts: 0, startedAt: seed - 239000, memMB: 18.1, cpuPct: 0.9 },
    { id: 'm3-env', name: 'm3-env', ns: 'default', container: 'shell', image: 'alpine:latest', phase: 'Running', ready: true, restarts: 2, startedAt: seed - 93600000, memMB: 9.3, cpuPct: 0.2 },
  ],
  podFilter: 'all',
  images: [
    { id: 'alpine:latest', name: 'alpine:latest', layers: 1, comp: 4190595, unpacked: 8653198, pulledAt: '16:25', digest: 'sha256:abbc135a7d3909a5eb6a56aac4c769cf15be025e3c9e226b8660f2b2b378158b', registry: 'registry.example.com', inUse: ['demo-log', 'm3-env', 'hello-android'] },
    { id: 'hashicorp/http-echo:1.0', name: 'hashicorp/http-echo:1.0', layers: 10, comp: 4382140, unpacked: 8490000, pulledAt: '09:20', digest: 'sha256:5313e22e471645c8834796330f3f4443a86ceaa65db60b01ad4b2b0b5c619525', registry: 'registry.example.com', inUse: ['demo-web'] },
    { id: 'nginx:1.27-alpine', name: 'nginx:1.27-alpine', layers: 8, comp: 21810000, unpacked: 49700000, pulledAt: '昨天', digest: 'sha256:96868d9fa38f4a1d6f1d5a1f8e5f1c1d2b0a9c4e7f3d2a1b0c9e8d7f6a5b4c3d', registry: 'registry.example.com', inUse: [] },
    { id: 'python:3.12-slim', name: 'python:3.12-slim', layers: 4, comp: 43510000, unpacked: 144800000, pulledAt: '昨天', digest: 'sha256:3617028275c0e6d3b1f9a8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7', registry: 'registry.example.com', inUse: [] },
    { id: 'rancher/mirrored-metrics-server:v0.9.0', name: 'rancher/mirrored-metrics-server:v0.9.0', layers: 4, comp: 34700000, unpacked: 65700000, pulledAt: '11-10', digest: 'sha256:37cda8805d2a1e0f9b8c7d6e5f4a3b2c1d0e9f8a7b6c5d4e3f2a1b0c9d8e7f6a', registry: 'registry.example.com', inUse: [] },
  ],
  pulling: null,
  logs: [
    { t: '16:21:56', level: 'INFO', msg: 'node_agent: droidnode agent starting node_id=droidnode-37e62e0fe8ce0057' },
    { t: '16:21:56', level: 'INFO', msg: 'node_agent: no orphan proot processes found' },
    { t: '16:21:56', level: 'INFO', msg: 'node_agent: proot smoke test: starting' },
    { t: '16:21:56', level: 'INFO', msg: 'registry_mirror: registry mirror applied from=registry-1.docker.io to=registry.example.com' },
    { t: '16:21:56', level: 'INFO', msg: 'image_orchestration_service: rootfs already prepared (IfNotPresent) image=library/alpine:latest' },
    { t: '16:21:56', level: 'INFO', msg: 'reconciliation_service: reconciliation loop started node=droidnode-37e62e0fe8ce0057' },
    { t: '16:21:56', level: 'INFO', msg: 'node_registration_service: registering node with control plane node_id=droidnode-37e62e0fe8ce0057' },
    { t: '16:21:56', level: 'WARN', msg: 'virtual_kubelet_exposer: kubelet CA (re)written path=/data/user/0/com.droidnode/files/kubelet-ca.crt' },
    { t: '16:22:07', level: 'INFO', msg: 'workload_lifecycle_service: POD_EVENT started pod=demo-log' },
    { t: '16:22:07', level: 'INFO', msg: 'workload_lifecycle_service: POD_EVENT started pod=demo-web' },
    { t: '16:25:13', level: 'INFO', msg: 'image_store: image stored name=alpine:latest rootfs=/data/user/0/com.droidnode/files/rootfs/registry.example.com/library/alpine/latest' },
    { t: '16:26:41', level: 'ERROR', msg: 'workload_lifecycle_service: failed to start pod=hello-android: exec /bin/sh: no such file or directory' },
  ],
  logPaused: false,
  logLevel: 'all',
  logQuery: '',
  logPod: null,
  newLogs: 0,
  sheet: null,
  settings: {
    nodeName: NODE.name,
    port: '10255',
    mirrors: { docker: 'registry.example.com', k8s: '', quay: '' },
    registry: NODE.registry,
    logLevel: 'info',
    autostart: true,
    battery: true,
    keepOnline: true,
    appearance: 'light',
    language: 'zh',
    configHint: '',
    iface: '',
  },
  backTo: 'settings',
  reportAt: Date.now(),
  kubeconfig: { imported: true, server: NODE.server, auth: 'token' },
  join: {
    mode: 'token',
    server: NODE.server,
    token: '',
    ca: '',
    kubeText: '',
    note: '',
    noteTone: 'idle',
    busy: false,
  },
  test: 'idle',
  resources: {
    memAvail: 6310494208,
    memTotal: 12000000000,
    procs: 7,
    rssMB: 238,
    cpuCores: 8,
    cpuPct: 24.7,
    diskAvail: 53717295104,
    diskTotal: 241505894400,
    battery: 100,
    charging: true,
    temp: 31.2,
  },
};

/* ── 工具 ─────────────────────────────────────────────────── */

const $ = (sel) => document.querySelector(sel);

const icon = (name, cls = '') => `<svg class="i ${cls}" aria-hidden="true"><use href="#i-${name}"/></svg>`;

/* 配色：night（深色）/ paper（亮色）。令牌在 styles.css 里整套换。 */
const THEMES = ['night', 'paper'];

function applyTheme(id) {
  const theme = THEMES.includes(id) ? id : 'night';
  document.documentElement.dataset.theme = theme;
  document.querySelectorAll('[data-act="theme"]').forEach((btn) => {
    btn.setAttribute('aria-pressed', String(btn.dataset.theme === theme));
  });
  try {
    localStorage.setItem('droidnode.theme', theme);
  } catch {
    /* 隐私模式下 localStorage 可能不可用 */
  }
}

/** 设置页的「外观」：light / dark / system → 具体配色（跟随时看系统偏好） */
function resolveAppearance(mode) {
  if (mode === 'dark') return 'night';
  if (mode === 'light') return 'paper';
  return window.matchMedia && window.matchMedia('(prefers-color-scheme: dark)').matches ? 'night' : 'paper';
}

function esc(s) {
  return String(s).replace(/[&<>"']/g, (c) => ({ '&': '&amp;', '<': '&lt;', '>': '&gt;', '"': '&quot;', "'": '&#39;' }[c]));
}

function humanBytes(n) {
  if (!n || n <= 0) return '0 B';
  const units = ['B', 'KB', 'MB', 'GB'];
  let v = n;
  let i = 0;
  while (v >= 1024 && i < units.length - 1) {
    v /= 1024;
    i += 1;
  }
  return i === 0 ? `${Math.round(v)} B` : `${v.toFixed(1)} ${units[i]}`;
}

function uptimeText(ms) {
  const total = Math.max(0, Math.floor(ms / 1000));
  const h = Math.floor(total / 3600);
  const m = Math.floor((total % 3600) / 60);
  const s = total % 60;
  if (h > 0) return `${h} 小时 ${m} 分`;
  if (m > 0) return `${m} 分 ${String(s).padStart(2, '0')} 秒`;
  return `${s} 秒`;
}

function fmtAge(startedAt) {
  const ms = Date.now() - startedAt;
  const s = Math.floor(ms / 1000);
  if (s < 60) return `${s} 秒`;
  const m = Math.floor(s / 60);
  if (m < 60) return `${m} 分`;
  const h = Math.floor(m / 60);
  if (h < 24) return `${h} 小时`;
  return `${Math.floor(h / 24)} 天`;
}

function nowTime() {
  const d = new Date();
  return `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}:${String(d.getSeconds()).padStart(2, '0')}`;
}

function phaseOf(pod) {
  return PHASES[pod.phase] || { label: pod.phase, dot: 'lamp' };
}

const severity = (pod) => ({ ok: 2, lamp: 1, bad: 0 }[phaseOf(pod).dot] ?? 1);

function sortedPods(list) {
  return [...list].sort((a, b) => severity(a) - severity(b) || a.name.localeCompare(b.name));
}

function imageTotal() {
  return state.images.reduce((sum, i) => sum + i.comp + i.unpacked, 0);
}

function toast(msg) {
  const el = $('#toast');
  el.textContent = msg;
  el.classList.add('is-open');
  clearTimeout(toast.timer);
  toast.timer = setTimeout(() => el.classList.remove('is-open'), 2400);
}

async function copyText(text) {
  try {
    await navigator.clipboard.writeText(text);
    toast('已复制节点名');
  } catch {
    toast('复制失败，请长按选择文本');
  }
}

/* ── 顶部与底栏 ───────────────────────────────────────────── */

function renderPill() {
  const pill = $('#node-pill');
  let dot = 'ok';
  let text = '运行中';
  if (state.agent === 'restarting') {
    dot = 'lamp';
    text = '启动中';
  } else if (state.agent === 'stopped') {
    dot = 'bad';
    text = '已停止';
  } else if (state.maintained) {
    dot = 'lamp';
    text = '已排空';
  }
  pill.innerHTML = `<span class="dot dot--${dot}"></span>${text}`;
}

function renderTabs() {
  $('#tabbar').innerHTML = TABS.map(
    (t) => `<button class="tab" role="tab" aria-selected="${state.screen === t.id}" data-act="tab" data-screen="${t.id}">
      ${icon(t.icon)}
      <span>${t.label}</span>
    </button>`,
  ).join('');
}

/* ── 节点页 ───────────────────────────────────────────────── */

const PULSE = 'M0 16 h40 l5 -9 l6 18 l5 -9 h44 l5 -9 l6 18 l5 -9 h44 l5 -9 l6 18 l5 -9 h64';

function heroState() {
  if (state.agent === 'restarting') return { dot: 'lamp', text: '正在启动', pulse: 'is-hold' };
  if (state.agent === 'stopped') return { dot: 'bad', text: '节点已停止', pulse: 'is-stop' };
  if (state.maintained) return { dot: 'lamp', text: '节点已排空', pulse: 'is-hold' };
  return { dot: 'ok', text: '节点运行中', pulse: '' };
}

function resourceRow(key, label, value, note, meterPct, tone) {
  const meter = typeof meterPct === 'number'
    ? `<div class="meter ${tone ? `is-${tone}` : ''}"><i style="width:${Math.min(100, Math.max(0, meterPct))}%" data-res-bar="${key}"></i></div>`
    : '';
  return `<div class="resource">
    <div class="resource-top">
      <span class="resource-label">${label}</span>
      <span class="resource-value" data-res="${key}">${value}</span>
    </div>
    ${meter}
    ${note ? `<div class="resource-note" data-res-note="${key}">${note}</div>` : ''}
  </div>`;
}

function screenNode() {
  const hero = heroState();
  const r = state.resources;
  const pods = sortedPods(state.pods);
  const ready = state.pods.filter((p) => p.phase === 'Running').length;
  const memUsedPct = Math.round(((r.memTotal - r.memAvail) / r.memTotal) * 100);
  const diskUsedPct = Math.round(((r.diskTotal - r.diskAvail) / r.diskTotal) * 100);

  const sub = state.agent === 'stopped'
    ? '前台服务未运行 · 节点已从集群掉线'
    : state.maintained
      ? `已运行 <span data-uptime>${uptimeText(Date.now() - state.startedAt)}</span> · 维护中，不再接收新的 Pod`
      : `已运行 <span data-uptime>${uptimeText(Date.now() - state.startedAt)}</span> · ${ready} / ${state.pods.length} 个 Pod 就绪`;

  return `
    <div class="hero">
      <div class="hero-top">
        <div>
          <div class="hero-state"><span class="dot dot--${hero.dot}"></span>${hero.text}</div>
          <div class="hero-sub">${sub}</div>
        </div>
        <div class="hero-actions">
          <button class="icon-btn icon-btn--bare" data-act="copy-node" aria-label="复制节点名">${icon('copy', 'i--s')}</button>
        </div>
      </div>

      <svg class="pulse-live ${hero.pulse}" viewBox="0 0 240 32" preserveAspectRatio="none" aria-hidden="true">
        <path class="echo" d="${PULSE}"/>
        <path class="trace" d="${PULSE}"/>
      </svg>

      <dl class="kv">
        <dt>节点名</dt>
        <dd>${esc(NODE.name)}</dd>
        <dd></dd>
      </dl>
      <dl class="kv">
        <dt>控制面</dt>
        <dd>${esc(state.kubeconfig.server)}</dd>
        <dd></dd>
      </dl>
      <dl class="kv">
        <dt>kubelet</dt>
        <dd>${esc(NODE.kubelet)}</dd>
        <dd></dd>
      </dl>
      <dl class="kv">
        <dt>本机 IP</dt>
        <dd>${esc(NODE.ip)}</dd>
        <dd></dd>
      </dl>
    </div>

    ${state.maintained && state.agent === 'running' ? `
    <button class="notice" data-act="tab" data-screen="settings">
      <span class="notice-main">维护中
        <span class="notice-sub">调度已停止，容器已排空</span>
      </span>
      <span class="notice-side">去设置 ${icon('chevron', 'i--s')}</span>
    </button>` : ''}

    <div class="sec-head">
      <svg class="tick" viewBox="0 0 24 8" aria-hidden="true"><path d="M0 4h6l2-3 3 6 2-3h11" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
      <h2>资源</h2>
      <span class="spacer"></span>
      <span class="card-note mono">每 3 秒刷新</span>
    </div>

    <div class="card">
      ${resourceRow('mem', '内存', `可用 ${humanBytes(r.memAvail)} / ${humanBytes(r.memTotal)}`, `已用 ${memUsedPct}%`, memUsedPct, memUsedPct > 85 ? 'bad' : memUsedPct > 70 ? 'warn' : '')}
      ${resourceRow('cpu', 'CPU', `${r.cpuCores} 核 · 本应用 ${r.cpuPct.toFixed(1)}%`, '本应用占单核的比例', null, '')}
      ${resourceRow('app', '本应用', `${r.procs} 个进程 · ${r.rssMB} MB`, '含 agent 与全部 proot 容器', null, '')}
      ${resourceRow('disk', '存储', `可用 ${humanBytes(r.diskAvail)} / ${humanBytes(r.diskTotal)}`, `已用 ${diskUsedPct}%`, diskUsedPct, diskUsedPct > 85 ? 'bad' : '')}
      ${resourceRow('battery', '电池', `${r.battery}% · ${r.charging ? '充电中' : '放电中'}`, `${r.temp.toFixed(1)}°C`, null, '')}
    </div>

    <div class="sec-head">
      <svg class="tick" viewBox="0 0 24 8" aria-hidden="true"><path d="M0 4h6l2-3 3 6 2-3h11" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
      <h2>Pod</h2>
      <span class="spacer"></span>
      <button class="link" data-act="tab" data-screen="pods">全部 ${state.pods.length} ${icon('chevron', 'i--s')}</button>
    </div>

    <div class="list">
      ${pods.length
        ? pods.slice(0, 3).map(podRow).join('')
        : `<div class="empty">
            <svg class="pulse-flat" viewBox="0 0 240 32" preserveAspectRatio="none" aria-hidden="true"><path d="M0 16h96l6-10 8 20 6-10h124"/></svg>
            <p>还没有 Pod 调度到本机。在控制面上部署一个工作负载就会出现在这里。</p>
          </div>`}
    </div>
  `;
}

function podRow(p) {
  const ph = phaseOf(p);
  const meta = p.restarts > 0 ? `重启 ${p.restarts} 次 · ${fmtAge(p.startedAt)}` : fmtAge(p.startedAt);
  return `<button class="row" data-act="pod" data-id="${esc(p.id)}">
    <span class="dot dot--${ph.dot}"></span>
    <span class="row-main">
      <span class="row-title">${esc(p.name)}</span>
      <span class="row-sub mono">${esc(p.container)} · ${esc(p.image)}</span>
    </span>
    <span class="row-side">
      <span class="row-state">${ph.label}</span>
      <span class="row-meta mono">${meta}</span>
    </span>
    ${icon('chevron', 'i--s')}
  </button>`;
}

/* ── Pod 页 ───────────────────────────────────────────────── */

function screenPods() {
  const abnormal = state.pods.filter((p) => phaseOf(p).dot === 'bad').length;
  const counts = { all: state.pods.length, running: state.pods.filter((p) => p.phase === 'Running').length, abnormal };
  const filters = [
    { id: 'all', label: '全部', count: counts.all },
    { id: 'running', label: '运行中', count: counts.running },
    { id: 'abnormal', label: '异常', count: abnormal },
  ];

  let list = sortedPods(state.pods);
  if (state.podFilter === 'running') list = list.filter((p) => p.phase === 'Running');
  if (state.podFilter === 'abnormal') list = list.filter((p) => phaseOf(p).dot === 'bad');

  const body = list.length
    ? `<div class="list">${list.map(podRow).join('')}</div>`
    : `<div class="empty">
        <svg class="pulse-flat" viewBox="0 0 240 32" preserveAspectRatio="none" aria-hidden="true"><path d="M0 16h96l6-10 8 20 6-10h124"/></svg>
        <p>这个筛选下没有 Pod</p>
        <button class="btn btn--ghost" data-act="pod-filter" data-filter="all">清除筛选</button>
      </div>`;

  return `
    <div class="filters" role="group" aria-label="按状态过滤">
      ${filters.map((f) => `<button class="chip" data-act="pod-filter" data-filter="${f.id}" aria-pressed="${state.podFilter === f.id}">
        ${f.label}<span class="count">${f.count}</span>
      </button>`).join('')}
    </div>
    ${body}
  `;
}

/* ── 镜像页 ───────────────────────────────────────────────── */

function imageRow(img) {
  const inUse = img.inUse.length > 0;
  const side = inUse
    ? `<span class="row-state"><span class="dot dot--lamp"></span>使用中</span>`
    : `<span class="row-meta mono">${img.layers} 层</span>`;
  return `<button class="row" data-act="image" data-id="${esc(img.id)}">
    <span class="row-main">
      <span class="row-title">${esc(img.name)}</span>
      <span class="row-sub mono">${humanBytes(img.comp)} → ${humanBytes(img.unpacked)}</span>
    </span>
    <span class="row-side">${side}</span>
    ${icon('chevron', 'i--s')}
  </button>`;
}

function screenImages() {
  const total = imageTotal();
  const pulling = state.pulling
    ? `<div class="list pull-card">
        <div class="row is-static">
          <svg class="tick" viewBox="0 0 24 8" aria-hidden="true"><path d="M0 4h6l2-3 3 6 2-3h11" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
          <span class="row-main">
            <span class="row-title">${esc(state.pulling.ref)}</span>
            <span class="row-sub">拉取中 · <span class="mono" data-pull-pct>0%</span></span>
          </span>
        </div>
        <div class="pull-bar"><i data-pull-bar></i></div>
      </div>`
    : '';

  const list = state.images.length
    ? `<div class="list">${state.images.map(imageRow).join('')}</div>`
    : `<div class="empty">
        <svg class="pulse-flat" viewBox="0 0 240 32" preserveAspectRatio="none" aria-hidden="true"><path d="M0 16h96l6-10 8 20 6-10h124"/></svg>
        <p>还没有镜像。部署一个 Pod 后，镜像会出现在这里。</p>
        <button class="btn btn--ghost" data-act="ask-pull">拉取镜像</button>
      </div>`;

  return `
    <div class="card">
      <div class="card-head">
        <div class="row-main">
          <span class="card-title">${state.images.length} 个镜像</span>
          <span class="card-note mono">共 ${humanBytes(total)}</span>
        </div>
        <button class="btn btn--ghost" data-act="ask-pull">${icon('plus', 'i--s')}拉取</button>
      </div>
      <p class="hint mono">${esc(state.settings.registry)}</p>
    </div>
    ${pulling}
    ${list}
  `;
}

/* ── 日志页 ───────────────────────────────────────────────── */

const LOG_LEVELS = [
  { id: 'all', label: '全部' },
  { id: 'INFO', label: 'INFO' },
  { id: 'WARN', label: 'WARN' },
  { id: 'ERROR', label: 'ERROR' },
];

function visibleLogs() {
  const q = state.logQuery.trim().toLowerCase();
  return state.logs.filter((l) => {
    if (state.logLevel !== 'all' && l.level !== state.logLevel) return false;
    if (state.logPod && !l.msg.includes(state.logPod)) return false;
    if (q && !l.msg.toLowerCase().includes(q) && !l.t.includes(q)) return false;
    return true;
  });
}

function logRows() {
  const rows = visibleLogs();
  if (!rows.length) {
    const empty = state.logs.length === 0;
    return `<div class="empty">
      <svg class="pulse-flat" viewBox="0 0 240 32" preserveAspectRatio="none" aria-hidden="true"><path d="M0 16h96l6-10 8 20 6-10h124"/></svg>
      <p>${empty ? '日志已清空。新的日志会继续追加。' : '没有匹配的日志'}</p>
    </div>`;
  }
  return rows
    .slice(-300)
    .map(
      (l) => `<div class="log-row" data-level="${l.level}">
        <span class="log-time">${l.t}</span>
        <span class="log-level" data-level="${l.level}">${l.level}</span>
        <span class="log-msg">${esc(l.msg)}</span>
      </div>`,
    )
    .join('');
}

function screenLogs() {
  const podChip = state.logPod
    ? `<button class="chip" data-act="clear-log-pod" aria-pressed="true">${esc(state.logPod)} ×</button>`
    : '';
  return `
    <div class="toolbar">
      <div class="search">
        ${icon('search', 'i--s')}
        <input class="input" type="search" data-model="logQuery" value="${esc(state.logQuery)}" placeholder="搜索日志" aria-label="搜索日志" />
      </div>
      <button class="icon-btn" data-act="toggle-pause" aria-label="${state.logPaused ? '继续' : '暂停'}">${icon(state.logPaused ? 'play' : 'pause', 'i--s')}</button>
    </div>
    <div class="filters" role="group" aria-label="按级别过滤">
      ${LOG_LEVELS.map((l) => `<button class="chip" data-act="log-level" data-level="${l.id}" aria-pressed="${state.logLevel === l.id}">${l.label}</button>`).join('')}
      ${podChip}
      <button class="chip" data-act="clear-logs">清空</button>
    </div>
    <div class="log-list" id="log-list">${logRows()}</div>
    ${state.logPaused && state.newLogs > 0
      ? `<button class="btn btn--primary btn--block" data-act="resume-logs">继续 · ${state.newLogs} 条新日志</button>`
      : ''}
  `;
}

function renderLogList() {
  const list = $('#log-list');
  if (!list) return;
  list.innerHTML = logRows();
  if (!state.logPaused) scrollLogs();
}

function scrollLogs() {
  const list = $('#log-list');
  if (list) list.scrollTop = list.scrollHeight;
}

/* ── 设置页 ───────────────────────────────────────────────── */

function settingRow(title, sub, control) {
  return `<div class="setting-row">
    <div class="row-main">
      <span class="row-title">${title}</span>
      <span class="row-sub">${sub}</span>
    </div>
    ${control}
  </div>`;
}

function appearanceChip(mode, label) {
  return `<button class="chip" data-act="appearance" data-mode="${mode}" aria-pressed="${state.settings.appearance === mode}">${label}</button>`;
}

function ifaceChip(iface, label) {
  return `<button class="chip" data-act="iface" data-iface="${iface}" aria-pressed="${state.settings.iface === iface}">${label}</button>`;
}

function languageChip(lang, label) {
  return `<button class="chip" data-act="language" data-lang="${lang}" aria-pressed="${state.settings.language === lang}">${label}</button>`;
}

function ifaceIp(iface) {
  return { wlan0: '192.0.2.11', 'p2p-p2p0-0': '198.51.100.27' }[iface] || '—';
}

/** 对应设置页「agent 上报」一行：Ready/NotReady · Pod · 连接 · N 秒前 */
function agentReportText() {
  const running = state.pods.filter((p) => p.phase === 'Running').length;
  const ready = state.agent === 'stopped' ? 'NotReady' : state.maintained ? 'NotReady（已排空）' : 'Ready';
  const link = state.agent === 'stopped' ? '已断开' : '已连接';
  const age = Math.max(0, Math.round((Date.now() - state.reportAt) / 1000));
  return `${ready} · ${running} 个 Pod 运行中 · ${link} · ${age} 秒前`;
}

/* ── 设置：一个状态摘要 + 五个二级页 ───────────────────────── */

function agentStatus() {
  if (state.agent === 'stopped') return { tone: 'bad', text: '已停止' };
  if (state.agent === 'restarting') return { tone: 'lamp', text: '启动中' };
  if (state.maintained) return { tone: 'lamp', text: '维护中（已 cordon + 排空）' };
  return { tone: 'ok', text: '运行中' };
}

function runningPodCount() {
  return state.pods.filter((p) => p.phase === 'Running').length;
}

function appearanceLabel() {
  const a = state.settings.appearance;
  return a === 'system' ? '跟随系统' : a === 'dark' ? '深色' : '亮色';
}

function languageLabel() {
  const l = state.settings.language;
  return l === 'system' ? '跟随系统' : l === 'en' ? 'English' : '简体中文';
}

function connectTestHint() {
  if (state.test === 'testing') return '<span class="hint">正在连接 6443…</span>';
  if (state.test === 'ok') return '<span class="hint hint--ok">连接正常 · /version 返回 200</span>';
  if (state.test === 'fail') return '<span class="hint hint--bad">连接失败 · 检查地址与 token</span>';
  return '';
}

/** 首页入口行：标题 + 当前值摘要 + 箭头。摘要在外面算，避免每页重复取值逻辑。 */
function settingsEntry(e) {
  const sub = typeof e.sub === 'function' ? e.sub() : e.sub;
  return `<button class="setting-row setting-row--link" data-act="go" data-screen="${e.id}">
    <span class="row-main">
      <span class="row-title">${e.title}</span>
      <span class="row-sub">${sub}</span>
    </span>
    ${icon('chevron', 'i--s row-chev')}
  </button>`;
}

function screenSettings() {
  const st = agentStatus();
  const kc = state.kubeconfig;
  const entries = [
    {
      id: 'settings-node',
      title: '节点与网络',
      sub: () => `${state.settings.nodeName} · :${state.settings.port} · ${state.settings.iface ? `固定 ${state.settings.iface}` : '自动选网卡'}`,
    },
    {
      id: 'settings-cluster',
      title: '集群连接',
      sub: () => (kc.imported ? `已加入 · ${kc.server}` : '未加入 · 节点无法注册'),
      tone: () => (kc.imported ? 'ok' : 'bad'),
    },
    {
      id: 'settings-runtime',
      title: '运行时与维护',
      sub: () => `${st.text} · ${runningPodCount()} 个 Pod`,
      tone: () => st.tone,
    },
    {
      id: 'settings-power',
      title: '电源与后台',
      sub: () => [
        state.settings.battery ? '电池已豁免' : '电池未豁免',
        state.settings.autostart ? '开机自启' : '不自启',
        state.settings.keepOnline ? '锁屏保持' : '锁屏省电',
      ].join(' · '),
      tone: () => (state.settings.battery ? 'ok' : 'lamp'),
    },
    {
      id: 'settings-app',
      title: '应用与诊断',
      sub: () => `${appearanceLabel()} · ${languageLabel()} · 0.1.60`,
    },
  ];

  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">这台手机节点</span>
        <span class="card-note"><span class="dot dot--${st.tone}"></span> ${st.text}</span>
      </div>
      ${settingRow('节点', `<span class="mono">${esc(state.settings.nodeName)}</span>`, `<span class="hint">${state.maintained ? 'NotReady' : 'Ready'}</span>`)}
      ${settingRow('控制面', kc.imported ? `<span class="mono">${esc(kc.server)}</span>` : '未加入', `<span class="dot dot--${kc.imported ? 'ok' : 'bad'}"></span>`)}
      <div class="toolbar">
        ${state.agent === 'running'
          ? `<button class="btn btn--quiet" data-act="ask-restart">重启 agent</button>
             <button class="btn btn--ghost" data-act="ask-stop">停止</button>`
          : `<button class="btn btn--primary" data-act="start-agent">启动 agent</button>`}
      </div>
    </div>

    <div class="sec-head">
      <svg class="tick" viewBox="0 0 24 8" aria-hidden="true"><path d="M0 4h6l2-3 3 6 2-3h11" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>
      <h2>设置</h2>
    </div>

    <div class="card">
      ${entries.map(settingsEntry).join('')}
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>关于</h2>
    </div>
    <div class="card">
      ${settingRow('版本', '0.1.60 · build 61', '')}
      ${settingRow('许可', 'MIT · 新界面控制台', '')}
      ${settingRow('源码', '<span class="mono">github.com/c0d3g3n13/droidnode</span>', '')}
    </div>
  `;
}

/* ── 设置 › 节点与网络 ───────────────────────────────────── */

function screenSettingsNode() {
  const s = state.settings;
  return `
    <div class="notice">
      <span class="notice-main">改这里会重启 agent
        <span class="notice-sub">改名会在集群里新建一个 Node，旧节点要手动删；kubelet 端口必须 ≥1024。</span>
      </span>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>身份</h2>
    </div>
    <div class="card">
      <div class="field">
        <label for="f-node">节点名</label>
        <input class="input" id="f-node" data-model="nodeName" value="${esc(s.nodeName)}" />
        <span class="hint">留空则用设备指纹自动生成，例如 <span class="mono">dn-k10</span></span>
      </div>
      <div class="field">
        <label for="f-port">kubelet 端口</label>
        <input class="input" id="f-port" data-model="port" value="${esc(s.port)}" inputmode="numeric" />
        <span class="hint" data-port-hint>重启 agent 后生效；低于 1024 会被系统拒绝</span>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>节点地址</h2>
    </div>
    <div class="card">
      <div class="field">
        <label>网卡</label>
        <div class="filters">
          ${ifaceChip('', '自动')}
          ${ifaceChip('wlan0', 'wlan0')}
          ${ifaceChip('p2p-p2p0-0', 'p2p')}
        </div>
        <span class="hint">${s.iface ? `固定 ${s.iface} · ${ifaceIp(s.iface)}` : '自动选择 · 跳过 Wi-Fi Direct / USB 共享'}；写 files/network-preference.env，agent 重启时生效。这张网卡没有地址时回退到自动选择。</span>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>镜像源</h2>
    </div>
    <div class="card">
      <div class="field">
        <label for="f-mirror-docker">docker.io</label>
        <input class="input" id="f-mirror-docker" data-model="mirrorDocker" value="${esc(s.mirrors.docker)}" placeholder="留空 = 直连上游" />
      </div>
      <div class="field">
        <label for="f-mirror-k8s">registry.k8s.io</label>
        <input class="input" id="f-mirror-k8s" data-model="mirrorK8s" value="${esc(s.mirrors.k8s)}" placeholder="留空 = 直连上游" />
      </div>
      <div class="field">
        <label for="f-mirror-quay">quay.io</label>
        <input class="input" id="f-mirror-quay" data-model="mirrorQuay" value="${esc(s.mirrors.quay)}" placeholder="留空 = 直连上游" />
        <span class="hint">生效：<span class="mono">${esc(s.registry)}</span> · 写 registry-mirrors.env</span>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>日志级别</h2>
    </div>
    <div class="card">
      <div class="field">
        <label for="f-level">agent 日志</label>
        <select class="select" id="f-level" data-model="logLevel">
          <option value="info" ${s.logLevel === 'info' ? 'selected' : ''}>info · 默认</option>
          <option value="debug" ${s.logLevel === 'debug' ? 'selected' : ''}>debug · 排障时开</option>
          <option value="warn" ${s.logLevel === 'warn' ? 'selected' : ''}>warn · 只看告警</option>
        </select>
        <span class="hint">debug 会让环形缓冲转得更快，日志页的有效历史变短</span>
      </div>
    </div>

    <div class="toolbar" style="margin-top: var(--sp-4)">
      <button class="btn btn--primary btn--block" data-act="save-config">保存并重启 agent</button>
    </div>
    <p class="hint" style="margin-top: var(--sp-2)" data-config-hint>${s.configHint || '节点名、端口、镜像源、日志级别都在 agent 启动时读取'}</p>
  `;
}

/* ── 设置 › 集群连接 ─────────────────────────────────────── */

function screenSettingsCluster() {
  const kc = state.kubeconfig;
  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">控制面</span>
        <span class="card-note"><span class="dot dot--${kc.imported ? 'ok' : 'bad'}"></span> ${kc.imported ? '已加入' : '未配置'}</span>
      </div>
      ${settingRow('地址', kc.imported ? `<span class="mono">${esc(kc.server)}</span>` : '未加入，节点无法注册', '')}
      ${settingRow('身份', kc.imported ? esc(kc.auth || 'token') : '—', '')}
      ${settingRow('文件', '<span class="mono">files/kubeconfig.enc</span>', '')}
      ${connectTestHint() ? `<div style="padding-bottom: var(--sp-3)">${connectTestHint()}</div>` : ''}
      <div class="toolbar">
        <button class="btn ${kc.imported ? 'btn--ghost' : 'btn--primary'}" data-act="go-join">${kc.imported ? '更换控制面' : '加入集群'}</button>
        <button class="btn btn--quiet" data-act="test-connect">连接测试</button>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>说明</h2>
    </div>
    <div class="card">
      ${settingRow('凭据怎么来', '控制面跑 make-kubeconfig.sh，或 kubectl create token', '')}
      ${settingRow('失败会怎样', '先验证再写盘，验证不过不覆盖现有 kubeconfig', '')}
      ${settingRow('401 与 403', '401 = 凭据不对；403 = 身份没权限（多半是 rbac.yaml 没重放）', '')}
    </div>
  `;
}

/* ── 设置 › 运行时与维护 ─────────────────────────────────── */

function screenSettingsRuntime() {
  const st = agentStatus();
  return `
    <div class="card">
      <div class="card-head">
        <span class="card-title">当前状态</span>
        <span class="card-note"><span class="dot dot--${st.tone}"></span> ${st.text}</span>
      </div>
      ${settingRow('agent 上报', `<span data-report>${agentReportText()}</span>`, '')}
      ${settingRow('调度', state.maintained || state.agent === 'stopped' ? '已停止接收新 Pod' : '正常接收 Pod', `<span class="hint mono">${state.maintained ? 'SchedulingDisabled' : 'SchedulingEnabled'}</span>`)}
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>维护模式</h2>
    </div>
    <div class="card">
      ${settingRow(
        '维护模式',
        state.agent === 'stopped'
          ? 'agent 已停止；启动后才能开启维护模式'
          : state.maintained
            ? '已 cordon 并排空：调度器不再投递新 Pod，本机容器已停止'
            : '开启后先 cordon 节点，再停止本机容器；关闭后恢复调度',
        `<label class="switch"><input type="checkbox" data-toggle="maintain" ${state.maintained ? 'checked' : ''} ${state.agent === 'stopped' ? 'disabled' : ''} aria-label="维护模式" /><span class="track"></span></label>`,
      )}
      <p class="hint" style="margin-top: var(--sp-2)">维护模式只停本机容器，节点仍在集群里；要彻底下线用下面的「停止 agent」。</p>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>进程</h2>
    </div>
    <div class="card">
      ${state.agent === 'running'
        ? settingRow(
            '重启 agent',
            '重新读取 kubeconfig 与节点配置；运行中的容器会重建',
            `<button class="btn btn--quiet" data-act="ask-restart">重启</button>`,
          )
        : settingRow(
            '启动 agent',
            '重新拉起前台服务与 agent 进程，节点会重新注册',
            `<button class="btn btn--primary" data-act="start-agent">启动</button>`,
          )}
      ${state.agent === 'running'
        ? settingRow(
            '停止 agent',
            '节点立即掉线，容器停止；停止过程会先 cordon、再停容器、最后删 Pod 对象',
            `<button class="btn btn--danger" data-act="ask-stop">停止</button>`,
          )
        : ''}
    </div>
    <p class="hint" style="margin-top: var(--sp-2)">停止后本节点会留下 <span class="mono">SchedulingDisabled</span>；下次启动自动解除。异常掉电时由控制面的清理任务收尾。</p>
  `;
}

/* ── 设置 › 电源与后台 ───────────────────────────────────── */

function screenSettingsPower() {
  const s = state.settings;
  return `
    <div class="card">
      ${settingRow(
        '电池优化白名单',
        s.battery ? '已加入，息屏后不会被系统清理' : '未加入，息屏后可能被系统清理',
        `<button class="btn btn--quiet" data-act="battery">系统设置</button>`,
      )}
      ${settingRow(
        '开机自启',
        '重启手机后自动拉起前台服务',
        `<label class="switch"><input type="checkbox" data-toggle="autostart" ${s.autostart ? 'checked' : ''} aria-label="开机自启" /><span class="track"></span></label>`,
      )}
      ${settingRow(
        '锁屏保持在线',
        s.keepOnline
          ? '一直持有唤醒锁：锁屏也不掉线，代价是耗电'
          : '空闲 60 秒释放唤醒锁：省电，但锁屏可能变 NotReady',
        `<label class="switch"><input type="checkbox" data-toggle="keepOnline" ${s.keepOnline ? 'checked' : ''} aria-label="锁屏保持在线" /><span class="track"></span></label>`,
      )}
    </div>

    <div class="notice" style="margin-top: var(--sp-3)">
      <span class="notice-main">白名单是唯一必须手动开的
        <span class="notice-sub">Doze 会忽略唤醒锁并冻结进程，没进白名单时应用内做什么都留不住心跳。</span>
      </span>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>系统里还要开这两处</h2>
    </div>
    <div class="card">
      ${settingRow('EMUI / 应用启动管理', '关闭「自动管理」，允许自启动、关联启动、后台活动', '')}
      ${settingRow('电池 · 更多设置', '打开「休眠时始终保持网络连接」', '')}
    </div>
  `;
}

/* ── 设置 › 应用与诊断 ───────────────────────────────────── */

function screenSettingsApp() {
  const s = state.settings;
  return `
    <div class="sec-head">
      ${tickSvg}
      <h2>外观</h2>
    </div>
    <div class="card">
      <div class="field">
        <label>配色</label>
        <div class="filters">
          ${appearanceChip('system', '跟随系统')}
          ${appearanceChip('light', '亮色')}
          ${appearanceChip('dark', '深色')}
        </div>
        <span class="hint">${s.appearance === 'system' ? '跟随系统的深色 / 浅色' : s.appearance === 'dark' ? '深色 · 夜间值守' : '亮色 · 白天更好读'}</span>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>语言</h2>
    </div>
    <div class="card">
      <div class="field">
        <label>界面语言</label>
        <div class="filters">
          ${languageChip('system', '跟随系统')}
          ${languageChip('zh', '简体中文')}
          ${languageChip('en', 'English')}
        </div>
        <span class="hint">APK 里切换后所有页面立即生效；原型只演示入口与选中态。</span>
      </div>
    </div>

    <div class="sec-head">
      ${tickSvg}
      <h2>诊断</h2>
    </div>
    <div class="card">
      <div class="toolbar">
        <button class="btn btn--ghost" data-act="export">导出诊断</button>
      </div>
      <p class="hint" style="margin-top: var(--sp-3)">版本、节点、外观与最近 200 行日志；走系统分享，不上传。</p>
    </div>
  `;
}

/* ── 加入集群（二级页） ───────────────────────────────────── */

const JOIN_MODES = [
  { id: 'token', title: '地址 + token', note: '手机自己从 /cacerts 取 CA' },
  { id: 'file', title: '导入 kubeconfig 文件', note: '控制面脚本生成的 yaml' },
  { id: 'paste', title: '粘贴 kubeconfig', note: '含管理员证书，仅排障用' },
];

const SAMPLE_KUBECONFIG = `apiVersion: v1
kind: Config
clusters:
- name: droidnode
  cluster:
    certificate-authority-data: LS0tLS1CRUdJTiBDRVJUSUZJQ0FURS0tLS0t
    server: https://192.0.2.10:6443
users:
- name: droidnode-agent
  user:
    token: eyJhbGciOiJSUzI1NiIsImtpZCI6ImRyb2lkbm9kZSJ9`;

const tickSvg = `<svg class="tick" viewBox="0 0 24 8" aria-hidden="true"><path d="M0 4h6l2-3 3 6 2-3h11" fill="none" stroke="currentColor" stroke-width="1.4" stroke-linecap="round" stroke-linejoin="round"/></svg>`;

function screenJoin() {
  const j = state.join;
  const tokenMode = j.mode === 'token';

  const options = JOIN_MODES.map((m) => `
    <button class="choice" data-act="join-mode" data-mode="${m.id}" aria-pressed="${j.mode === m.id}">
      <span class="choice-dot" aria-hidden="true"></span>
      <span>
        <span class="choice-title">${m.title}</span>
        <span class="choice-note">${m.note}</span>
      </span>
    </button>`).join('');

  const tokenForm = `
    <div class="field">
      <label for="j-server">控制面地址</label>
      <input class="input" id="j-server" data-model="join-server" value="${esc(j.server)}" inputmode="url" autocomplete="off" spellcheck="false" />
    </div>
    <div class="field">
      <label for="j-token">token</label>
      <input class="input" id="j-token" data-model="join-token" value="${esc(j.token)}" autocomplete="off" spellcheck="false" placeholder="kubectl create token …" />
    </div>
    <div class="field">
      <label for="j-ca">CA（选填）</label>
      <textarea class="textarea" id="j-ca" data-model="join-ca" placeholder="留空则从 /cacerts 取">${esc(j.ca)}</textarea>
    </div>`;

  const textForm = `
    <div class="field">
      <label for="j-kube">kubeconfig 全文</label>
      <textarea class="textarea" id="j-kube" data-model="join-text" placeholder="apiVersion: v1 …">${esc(j.kubeText)}</textarea>
      ${j.mode === 'file'
        ? `<button class="btn btn--ghost btn--block" data-act="join-pick-file">${icon('file', 'i--s')}从文件选择</button>`
        : '<span class="hint">管理员证书权限最大，且不能按设备吊销</span>'}
    </div>`;

  const note = j.note
    ? `<p class="join-note join-note--${j.noteTone}" role="status" aria-live="polite">${esc(j.note)}</p>`
    : '';

  return `
    <div class="sec-head">
      ${tickSvg}
      <h2>方式</h2>
    </div>
    <div class="choice-list">${options}</div>

    <div class="sec-head">
      ${tickSvg}
      <h2>凭据</h2>
    </div>
    <div class="card">${note}${tokenMode ? tokenForm : textForm}</div>`;
}

/* ── 弹层 ─────────────────────────────────────────────────── */

function sheetHead(title) {
  return `<div class="sheet-head">
    <h2>${title}</h2>
    <button class="icon-btn icon-btn--bare" data-act="close-sheet" aria-label="关闭">${icon('close', 'i--s')}</button>
  </div>`;
}

function sheetPod(pod) {
  const ph = phaseOf(pod);
  const events = state.logs.filter((l) => l.msg.includes(pod.name)).slice(-2);
  return `${sheetHead('Pod 详情')}
    <div class="sheet-body">
      <dl class="kv"><dt>名称</dt><dd>${esc(pod.name)}</dd><dd></dd></dl>
      <dl class="kv"><dt>命名空间</dt><dd>${esc(pod.ns)}</dd><dd></dd></dl>
      <dl class="kv"><dt>状态</dt><dd><span class="row-state"><span class="dot dot--${ph.dot}"></span>${ph.label}</span></dd><dd></dd></dl>
      <dl class="kv"><dt>容器</dt><dd>${esc(pod.container)}</dd><dd></dd></dl>
      <dl class="kv"><dt>镜像</dt><dd>${esc(pod.image)}</dd><dd></dd></dl>
      <dl class="kv"><dt>重启次数</dt><dd>${pod.restarts} 次</dd><dd></dd></dl>
      <dl class="kv"><dt>启动时间</dt><dd>${fmtAge(pod.startedAt)}前</dd><dd></dd></dl>
      <dl class="kv"><dt>Pod IP</dt><dd>${esc(NODE.ip)}</dd><dd></dd></dl>
      <dl class="kv"><dt>内存</dt><dd>${pod.memMB.toFixed(1)} MB</dd><dd></dd></dl>
      <dl class="kv"><dt>CPU</dt><dd>${pod.cpuPct.toFixed(1)}% 单核</dd><dd></dd></dl>
      ${events.length
        ? `<div class="sec-head"><h2>最近事件</h2></div>
           <div>${events.map((l) => `<div class="log-row" data-level="${l.level}"><span class="log-time">${l.t}</span><span class="log-level" data-level="${l.level}">${l.level}</span><span class="log-msg">${esc(l.msg)}</span></div>`).join('')}</div>`
        : ''}
    </div>
    <div class="sheet-actions">
      <button class="btn btn--primary" data-act="view-logs" data-id="${esc(pod.id)}">查看日志</button>
      <button class="btn btn--quiet" data-act="ask-delete-pod" data-id="${esc(pod.id)}">删除 Pod</button>
    </div>`;
}

function sheetImage(img) {
  return `${sheetHead('镜像详情')}
    <div class="sheet-body">
      <dl class="kv"><dt>镜像</dt><dd>${esc(img.name)}</dd><dd></dd></dl>
      <dl class="kv"><dt>digest</dt><dd>${esc(img.digest)}</dd><dd></dd></dl>
      <dl class="kv"><dt>层数</dt><dd>${img.layers} 层</dd><dd></dd></dl>
      <dl class="kv"><dt>压缩</dt><dd>${humanBytes(img.comp)}</dd><dd></dd></dl>
      <dl class="kv"><dt>解包</dt><dd>${humanBytes(img.unpacked)}</dd><dd></dd></dl>
      <dl class="kv"><dt>拉取时间</dt><dd>${esc(img.pulledAt)}</dd><dd></dd></dl>
      <dl class="kv"><dt>镜像源</dt><dd>${esc(img.registry)}</dd><dd></dd></dl>
      <p class="hint" style="margin-top: var(--sp-3)">${img.inUse.length ? `被 ${img.inUse.join('、')} 使用，删除会被拒绝` : '没有 Pod 使用，可以安全删除'}</p>
    </div>
    <div class="sheet-actions">
      <button class="btn ${img.inUse.length ? 'btn--quiet' : 'btn--danger'}" data-act="ask-delete-image" data-id="${esc(img.id)}" ${img.inUse.length ? 'disabled' : ''}>${img.inUse.length ? '使用中，不能删除' : `删除并释放 ${humanBytes(img.unpacked)}`}</button>
      <button class="btn btn--ghost" data-act="close-sheet">返回</button>
    </div>`;
}

function sheetConfirm(s) {
  return `${sheetHead(s.title)}
    <div class="sheet-body"><p>${s.body}</p></div>
    <div class="sheet-actions">
      <button class="btn ${s.danger ? 'btn--danger' : 'btn--primary'}" data-act="do-confirm">${s.confirmLabel}</button>
      <button class="btn btn--ghost" data-act="close-sheet">取消</button>
    </div>`;
}

function sheetPull() {
  return `${sheetHead('拉取镜像')}
    <div class="sheet-body">
      <div class="field">
        <label for="f-pull">镜像引用</label>
        <input class="input" id="f-pull" data-model="pullRef" placeholder="nginx:1.27-alpine" />
        <span class="hint" data-pull-hint>会从 ${esc(state.settings.registry)} 拉取，与 kubectl 里的 image 字段一致</span>
      </div>
    </div>
    <div class="sheet-actions">
      <button class="btn btn--primary" data-act="do-pull">开始拉取</button>
      <button class="btn btn--ghost" data-act="close-sheet">取消</button>
    </div>`;
}

function renderSheet() {
  const sheet = $('#sheet');
  const scrim = $('#scrim');
  const s = state.sheet;
  if (!s) {
    sheet.classList.remove('is-open');
    sheet.innerHTML = '';
    scrim.classList.remove('is-open');
    return;
  }
  $('#toast').classList.remove('is-open');
  let html = '';
  if (s.type === 'pod') {
    const pod = state.pods.find((p) => p.id === s.id);
    html = pod ? sheetPod(pod) : '';
  } else if (s.type === 'image') {
    const img = state.images.find((i) => i.id === s.id);
    html = img ? sheetImage(img) : '';
  } else if (s.type === 'confirm') {
    html = sheetConfirm(s);
  } else if (s.type === 'pull') {
    html = sheetPull();
  }
  sheet.innerHTML = html;
  sheet.classList.add('is-open');
  scrim.classList.add('is-open');
}

/* ── 右侧说明 ─────────────────────────────────────────────── */

function renderPanel() {
  const meta = SCREENS[state.screen];
  $('#panel').innerHTML = `
    <header class="panel-head">
      <h2>DroidNode 控制台</h2>
      <p>手机上的单节点控制台 · 原型 v0.1 · 现有 APK 与任何代码均未改动</p>
    </header>
    <section class="panel-sec">
      <h3>本页目的</h3>
      <p>${meta.purpose}</p>
    </section>
    <section class="panel-sec">
      <h3>数据来源</h3>
      <ul>${meta.source.map((x) => `<li>${x}</li>`).join('')}</ul>
    </section>
    <section class="panel-sec">
      <h3>关键交互</h3>
      <ul>${meta.actions.map((x) => `<li>${x}</li>`).join('')}</ul>
    </section>
    <section class="panel-sec">
      <h3>与现状差异</h3>
      <ul>${meta.diff.map((x) => `<li>${x}</li>`).join('')}</ul>
    </section>
    <footer class="panel-foot">
      ${SHOTS[state.screen] ? `<button class="btn btn--ghost" data-act="shot">对照当前 APK 截图</button>` : ''}
      <p class="hint">点亮每个状态、开关和弹层都可以直接操作，数据是本地模拟。</p>
    </footer>
  `;
}

/* ── 主渲染 ───────────────────────────────────────────────── */

const RENDERERS = {
  node: screenNode,
  pods: screenPods,
  images: screenImages,
  logs: screenLogs,
  settings: screenSettings,
  'settings-node': screenSettingsNode,
  'settings-cluster': screenSettingsCluster,
  'settings-runtime': screenSettingsRuntime,
  'settings-power': screenSettingsPower,
  'settings-app': screenSettingsApp,
  join: screenJoin,
};

/** 二级页：隐藏 tabbar 与节点 pill，顶部出现返回。 */
const SECONDARY = new Set([
  'join',
  'settings-node',
  'settings-cluster',
  'settings-runtime',
  'settings-power',
  'settings-app',
]);

function render() {
  const view = $('#view');
  const keep = view.scrollTop;
  const secondary = SECONDARY.has(state.screen);
  $('#screen-title').textContent = SCREENS[state.screen].title;
  $('#back-btn').hidden = !secondary;
  $('#node-pill').hidden = secondary;
  $('#tabbar').hidden = secondary;
  $('#join-foot').hidden = state.screen !== 'join';
  if (secondary) {
    const go = $('#join-go');
    go.textContent = state.join.mode === 'token' ? '加入' : '导入并加入';
    go.disabled = !!state.join.busy;
  }
  view.className = `view${state.screen === 'logs' ? ' view--fill' : ''}`;
  view.innerHTML = RENDERERS[state.screen]();
  view.scrollTop = secondary ? 0 : keep;
  renderPill();
  renderTabs();
  renderPanel();
  if (state.screen === 'logs') scrollLogs();
}

/* ── 行为 ─────────────────────────────────────────────────── */

/** 返回上一级：合入集群是「集群连接」的三级页，其余二级页都回到设置首页。 */
function secondaryParent() {
  const parent = state.backTo || 'settings';
  state.backTo = 'settings';
  return parent;
}

function setScreen(id) {
  if (!RENDERERS[id]) return;
  state.screen = id;
  state.sheet = null;
  renderSheet();
  render();
  $('#view').scrollTop = 0;
  if (location.hash !== `#${id}`) history.replaceState(null, '', `#${id}`);
}

function setMaintained(on) {
  if (state.agent === 'stopped') {
    toast('agent 已停止，先启动后再开启维护模式');
    render();
    return;
  }
  state.maintained = on;
  let stopped = 0;
  for (const p of state.pods) {
    if (on) {
      p.phaseBefore = p.phase;
      if (p.phase === 'Running') {
        p.phase = 'Drained';
        stopped += 1;
      }
    } else if (p.phase === 'Drained') {
      p.phase = p.phaseBefore || 'Running';
    }
  }
  toast(on ? `已 cordon 并排空 · 停止 ${stopped} 个容器` : '已恢复调度 · 容器将重新启动');
  render();
}

function askDeleteImage(id) {
  const img = state.images.find((i) => i.id === id);
  if (!img) return;
  state.sheet = {
    type: 'confirm',
    action: 'delete-image',
    id,
    title: '删除镜像',
    body: `删除 ${img.name} 会释放 ${humanBytes(img.unpacked)}，下次部署需要重新拉取。`,
    confirmLabel: '删除',
    danger: true,
  };
  renderSheet();
}

function deleteImage(id) {
  const img = state.images.find((i) => i.id === id);
  state.sheet = null;
  renderSheet();
  if (!img) return;
  state.images = state.images.filter((i) => i.id !== id);
  toast(`已删除 ${img.name} · 释放 ${humanBytes(img.unpacked)}`);
  render();
}

function startPull(ref) {
  state.sheet = null;
  renderSheet();
  state.pulling = { ref, pct: 0 };
  if (state.screen !== 'images') {
    state.screen = 'images';
  }
  render();
  const timer = setInterval(() => {
    if (!state.pulling) {
      clearInterval(timer);
      return;
    }
    state.pulling.pct = Math.min(100, state.pulling.pct + 6 + Math.random() * 10);
    const bar = document.querySelector('[data-pull-bar]');
    const pct = document.querySelector('[data-pull-pct]');
    if (bar) bar.style.width = `${state.pulling.pct}%`;
    if (pct) pct.textContent = `${Math.round(state.pulling.pct)}%`;
    if (state.pulling.pct >= 100) {
      clearInterval(timer);
      const name = state.pulling.ref;
      const comp = 6e6 + Math.round(Math.random() * 1e7);
      state.pulling = null;
      state.images.push({
        id: name,
        name,
        layers: 3 + Math.round(Math.random() * 6),
        comp,
        unpacked: comp * 2 + 1e6,
        pulledAt: '刚刚',
        digest: 'sha256:' + Array.from({ length: 64 }, () => '0123456789abcdef'[Math.floor(Math.random() * 16)]).join(''),
        registry: state.settings.registry,
        inUse: [],
      });
      toast(`已拉取 ${name}`);
      render();
    }
  }, 420);
}

function setAgentState(next) {
  state.agent = next;
  render();
  if (next === 'restarting') {
    setTimeout(() => {
      state.agent = 'running';
      state.startedAt = Date.now();
      if (state.maintained) {
        // Coming back into maintenance: containers stay drained.
        for (const p of state.pods) {
          if (p.phase === 'Running') {
            p.phaseBefore = p.phase;
            p.phase = 'Drained';
          }
        }
      }
      toast(state.maintained ? '已启动 · 节点仍在维护模式' : '已重启 · 节点已就绪');
      render();
    }, 2400);
  }
}

function testConnect() {
  state.test = 'testing';
  render();
  setTimeout(() => {
    state.test = 'ok';
    toast('连接正常');
    render();
  }, 1100);
}

/** 加入集群：先校验再"落盘"，失败只提示，不改动现有 kubeconfig。 */
function submitJoin() {
  const j = state.join;
  const server = j.server.trim();
  const missing = j.mode === 'token'
    ? !server || server === 'https:'
      ? '填控制面地址，例如 https://192.0.2.10:6443'
      : !j.token.trim()
        ? '填 token：kubectl -n kube-system create token droidnode-agent'
        : ''
    : !j.kubeText.trim()
      ? '先选文件，或粘贴 kubeconfig 全文'
      : !/server:\s*\S+/.test(j.kubeText)
        ? '没找到 server: 行，确认是完整 kubeconfig'
        : '';
  if (missing) {
    j.note = missing;
    j.noteTone = 'bad';
    render();
    return;
  }

  j.busy = true;
  j.note = j.mode === 'token'
    ? '正在从 /cacerts 取 CA，验证 TLS 与身份…'
    : '正在验证 TLS 与身份…';
  j.noteTone = 'idle';
  render();

  setTimeout(() => {
    j.busy = false;
    const srv = j.mode === 'token'
      ? server
      : (j.kubeText.match(/server:\s*(\S+)/) || [])[1] || NODE.server;
    state.kubeconfig = {
      imported: true,
      server: srv,
      auth: j.mode === 'token'
        ? 'token'
        : /client-certificate-data/.test(j.kubeText) ? '客户端证书' : 'token',
    };
    state.test = 'ok';
    j.note = '';
    toast(j.mode === 'token' ? '已加入 · agent 正在重启' : '已导入 kubeconfig · agent 正在重启');
    setScreen('settings');
  }, 1100);
}

function toggleLogLevel(level) {
  state.logLevel = level;
  document.querySelectorAll('[data-act="log-level"]').forEach((btn) => {
    btn.setAttribute('aria-pressed', String(btn.dataset.level === level));
  });
  renderLogList();
}

function openShot() {
  const shot = SHOTS[state.screen];
  if (!shot) {
    toast('这一页没有对应的旧版截图');
    return;
  }
  $('#shot').src = shot.path;
  $('#shot-cap').textContent = shot.caption;
  $('#lightbox').classList.add('is-open');
}

function closeOverlays() {
  state.sheet = null;
  renderSheet();
  $('#lightbox').classList.remove('is-open');
}

/* ── 事件 ─────────────────────────────────────────────────── */

document.addEventListener('click', (e) => {
  const el = e.target.closest('[data-act]');
  if (!el) return;
  const act = el.dataset.act;
  const id = el.dataset.id;

  switch (act) {
    case 'tab':
      setScreen(el.dataset.screen);
      break;
    case 'theme':
      applyTheme(el.dataset.theme);
      break;
    case 'back':
      setScreen(secondaryParent());
      break;
    case 'go':
      setScreen(el.dataset.screen);
      break;
    case 'go-join':
      state.backTo = 'settings-cluster';
      setScreen('join');
      break;
    case 'join-mode':
      state.join.mode = el.dataset.mode;
      state.join.note = '';
      render();
      break;
    case 'join-pick-file':
      state.join.kubeText = SAMPLE_KUBECONFIG;
      state.join.note = '已读取 droidnode-kubeconfig.yaml（原型模拟）';
      state.join.noteTone = 'ok';
      render();
      break;
    case 'join-submit':
      submitJoin();
      break;
    case 'close-sheet':
      state.sheet = null;
      renderSheet();
      break;
    case 'copy-node':
      copyText(NODE.name);
      break;
    case 'pod':
      state.sheet = { type: 'pod', id };
      renderSheet();
      break;
    case 'pod-filter':
      state.podFilter = el.dataset.filter;
      render();
      break;
    case 'view-logs': {
      const pod = state.pods.find((p) => p.id === id);
      state.logPod = pod ? pod.name : null;
      state.logLevel = 'all';
      state.logQuery = '';
      setScreen('logs');
      scrollLogs();
      break;
    }
    case 'image':
      state.sheet = { type: 'image', id };
      renderSheet();
      break;
    case 'ask-delete-image':
      askDeleteImage(id);
      break;
    case 'ask-delete-pod':
      toast('原型不发送删除请求');
      break;
    case 'do-confirm':
      if (state.sheet && state.sheet.action === 'delete-image') deleteImage(state.sheet.id);
      else if (state.sheet && state.sheet.action === 'restart-agent') {
        state.sheet = null;
        renderSheet();
        setAgentState('restarting');
      } else if (state.sheet && state.sheet.action === 'stop-agent') {
        state.sheet = null;
        renderSheet();
        setAgentState('stopped');
        toast('已停止 · 节点已从集群掉线');
      }
      break;
    case 'ask-pull':
      state.sheet = { type: 'pull' };
      renderSheet();
      break;
    case 'do-pull': {
      const input = document.querySelector('[data-model="pullRef"]');
      const ref = (input ? input.value : '').trim();
      if (!ref) {
        const hint = document.querySelector('[data-pull-hint]');
        if (hint) {
          hint.classList.add('hint--bad');
          hint.textContent = '先填镜像引用，例如 nginx:1.27-alpine';
        }
        break;
      }
      startPull(ref);
      break;
    }
    case 'test-connect':
      testConnect();
      break;
    case 'ask-restart':
      state.sheet = {
        type: 'confirm',
        action: 'restart-agent',
        title: '重启 agent',
        body: '会先停止前台服务里的 agent 进程，等容器退出后再拉起新的 agent。Pod 会被重新调度。',
        confirmLabel: '重启',
        danger: true,
      };
      renderSheet();
      break;
    case 'ask-stop':
      state.sheet = {
        type: 'confirm',
        action: 'stop-agent',
        title: '停止 agent',
        body: '节点会立即从集群掉线，所有容器停止。维护模式不会被自动关闭；之后可在设置页重新启动。',
        confirmLabel: '停止',
        danger: true,
      };
      renderSheet();
      break;
    case 'start-agent':
      setAgentState('restarting');
      toast('正在启动 agent…');
      break;
    case 'appearance':
      state.settings.appearance = el.dataset.mode;
      applyTheme(resolveAppearance(state.settings.appearance));
      render();
      toast(`外观：${el.textContent}`);
      break;
    case 'iface':
      state.settings.iface = el.dataset.iface;
      render();
      toast(el.dataset.iface ? `已固定到 ${el.dataset.iface}，重启 agent 生效` : '已改为自动选择');
      break;
    case 'language':
      state.settings.language = el.dataset.lang;
      render();
      toast(`${el.textContent}（原型只演示入口）`);
      break;
    case 'save-config':
      state.settings.configHint = '已保存（节点配置与镜像源），agent 正在按新配置重启';
      setAgentState('restarting');
      break;
    case 'export':
      toast('原型不导出文件；实际 APK 走系统分享');
      break;
    case 'battery':
      toast('原型不打开系统设置');
      break;
    case 'toggle-pause':
      state.logPaused = !state.logPaused;
      if (!state.logPaused) state.newLogs = 0;
      render();
      break;
    case 'resume-logs':
      state.logPaused = false;
      state.newLogs = 0;
      render();
      scrollLogs();
      break;
    case 'log-level':
      toggleLogLevel(el.dataset.level);
      break;
    case 'clear-log-pod':
      state.logPod = null;
      render();
      break;
    case 'clear-logs':
      state.logs = [];
      state.newLogs = 0;
      render();
      break;
    case 'shot':
      openShot();
      break;
    case 'close-lightbox':
      $('#lightbox').classList.remove('is-open');
      break;
    default:
      break;
  }
});

document.addEventListener('change', (e) => {
  const t = e.target.closest('[data-toggle]');
  if (t) {
    if (t.dataset.toggle === 'maintain') setMaintained(t.checked);
    if (t.dataset.toggle === 'autostart') {
      state.settings.autostart = t.checked;
      toast(t.checked ? '已开启开机自启' : '已关闭开机自启');
    }
    if (t.dataset.toggle === 'keepOnline') {
      state.settings.keepOnline = t.checked;
      toast(t.checked ? '锁屏会一直持有唤醒锁（更耗电）' : '空闲 60 秒释放唤醒锁（可能掉线）');
      render();
    }
    return;
  }
  const s = e.target.closest('[data-model="logLevel"]');
  if (s) {
    state.settings.logLevel = s.value;
    toast(`日志级别已改为 ${s.value}，重启 agent 后生效`);
  }
});

document.addEventListener('input', (e) => {
  const t = e.target.closest('[data-model]');
  if (!t) return;
  const model = t.dataset.model;
  if (model === 'logQuery') {
    state.logQuery = t.value;
    renderLogList();
  } else if (model === 'port') {
    state.settings.port = t.value;
    validatePort(t.value);
  } else if (model === 'nodeName') {
    state.settings.nodeName = t.value;
  } else if (model === 'registry') {
    state.settings.registry = t.value;
  } else if (model === 'mirrorDocker') {
    state.settings.mirrors.docker = t.value;
  } else if (model === 'mirrorK8s') {
    state.settings.mirrors.k8s = t.value;
  } else if (model === 'mirrorQuay') {
    state.settings.mirrors.quay = t.value;
  } else if (model === 'join-server') {
    state.join.server = t.value;
  } else if (model === 'join-token') {
    state.join.token = t.value;
  } else if (model === 'join-ca') {
    state.join.ca = t.value;
  } else if (model === 'join-text') {
    state.join.kubeText = t.value;
  }
});

document.addEventListener('keydown', (e) => {
  if (e.key === 'Escape') closeOverlays();
});

function validatePort(value) {
  const n = Number(value);
  const ok = Number.isInteger(n) && n >= 1024 && n <= 65535;
  const input = document.querySelector('[data-model="port"]');
  const hint = document.querySelector('[data-port-hint]');
  if (input) input.classList.toggle('is-bad', !ok);
  if (hint) {
    hint.classList.toggle('hint--bad', !ok);
    hint.textContent = ok ? '重启 agent 后生效' : '端口需为 1024 至 65535 之间的整数';
  }
  return ok;
}

/* ── 定时器：运行时长、资源、日志流 ───────────────────────── */

const LOG_POOL = [
  { level: 'INFO', msg: 'reconciliation_service: reconcile tick desired=4 actual=4 drifted=0' },
  { level: 'INFO', msg: 'node_registration_service: heartbeat sent to control plane node=droidnode-37e62e0fe8ce0057' },
  { level: 'INFO', msg: 'workload_lifecycle_service: container logs flushed pod=demo-web bytes=1832' },
  { level: 'INFO', msg: 'image_store: gc removed 0 unreferenced layers' },
  { level: 'WARN', msg: 'virtual_kubelet_exposer: containerLogs tailLines=200 requested namespace=default pod=demo-web' },
  { level: 'WARN', msg: 'reconciliation_service: pod hello-android in CrashLoopBackOff, next retry in 20s' },
  { level: 'INFO', msg: 'workload_lifecycle_service: POD_EVENT restarted pod=hello-android attempt=5' },
  { level: 'INFO', msg: 'proc_metrics: node memory 6.3 GiB available, 11.2 GiB total' },
];

let logPoolIndex = 0;

function pushLog() {
  const item = LOG_POOL[logPoolIndex % LOG_POOL.length];
  logPoolIndex += 1;
  state.logs.push({ t: nowTime(), level: item.level, msg: item.msg });
  if (state.logs.length > 500) state.logs.shift();
  if (state.logPaused) {
    state.newLogs += 1;
    if (state.screen === 'logs') {
      const active = document.activeElement;
      const typing = active && active.dataset && active.dataset.model === 'logQuery';
      const resume = document.querySelector('[data-act="resume-logs"]');
      if (typing) {
        if (resume) resume.textContent = `继续 · ${state.newLogs} 条新日志`;
      } else {
        render();
      }
    }
    return;
  }
  if (state.screen === 'logs') {
    renderLogList();
  }
}

function updateUptime() {
  const text = uptimeText(Date.now() - state.startedAt);
  document.querySelectorAll('[data-uptime]').forEach((el) => {
    el.textContent = text;
  });
}

function driftResources() {
  const r = state.resources;
  r.memAvail = Math.min(r.memTotal - 2e9, Math.max(3e9, r.memAvail + (Math.random() - 0.5) * 5e8));
  r.cpuPct = Math.min(64, Math.max(4, r.cpuPct + (Math.random() - 0.5) * 6));
  r.temp = Math.min(38, Math.max(29, r.temp + (Math.random() - 0.5) * 0.6));
  // 每 10 秒模拟一次 agent 上报，设置页的「N 秒前」跟着走
  if (Date.now() - state.reportAt > 10000) state.reportAt = Date.now();
  if (state.screen === 'node') render();
  if (state.screen === 'settings') {
    const row = document.querySelector('[data-report]');
    if (row) row.textContent = agentReportText();
  }
}

function updateClock() {
  const d = new Date();
  $('#clock').textContent = `${String(d.getHours()).padStart(2, '0')}:${String(d.getMinutes()).padStart(2, '0')}`;
}

/* ── 启动 ─────────────────────────────────────────────────── */

const savedTheme = (() => {
  try {
    return localStorage.getItem('droidnode.theme');
  } catch {
    return null;
  }
})();
applyTheme(savedTheme || 'night');

const initialScreen = location.hash.replace('#', '');
if (RENDERERS[initialScreen]) state.screen = initialScreen;

window.addEventListener('hashchange', () => {
  const id = location.hash.replace('#', '');
  if (RENDERERS[id] && id !== state.screen) setScreen(id);
});

render();
updateClock();
setInterval(updateUptime, 1000);
setInterval(driftResources, 3000);
setInterval(pushLog, 2600);
setInterval(updateClock, 30000);
