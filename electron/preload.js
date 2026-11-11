'use strict';
const { contextBridge, ipcRenderer } = require('electron');

contextBridge.exposeInMainWorld('fennec', {
  info: () => ipcRenderer.invoke('app:info'),
  openSetup: () => ipcRenderer.invoke('app:open-setup'),
  openWebWorkspace: () => ipcRenderer.invoke('app:open-workspace'),
  saveSettings: (patch) => ipcRenderer.invoke('settings:save', patch),

  setup: {
    detect: () => ipcRenderer.invoke('setup:detect'),
    pickFolder: (options) => ipcRenderer.invoke('setup:pick-folder', options),
    validateRoot: (dir) => ipcRenderer.invoke('setup:validate-root', dir),
    prerequisites: (payload) => ipcRenderer.invoke('setup:prerequisites', payload),
    installPrerequisite: (id) => ipcRenderer.invoke('setup:install-prerequisite', { id }),
    findPython: () => ipcRenderer.invoke('setup:find-python'),
    checkPython: (python) => ipcRenderer.invoke('setup:check-python', { python }),
    ensurePip: (python) => ipcRenderer.invoke('setup:ensure-pip', { python }),
    checkDeps: (python) => ipcRenderer.invoke('setup:check-deps', { python }),
    installDeps: (payload) => ipcRenderer.invoke('setup:install-deps', payload),
    validateEngine: (dir) => ipcRenderer.invoke('setup:validate-engine', dir),
    cloneEngine: (dir) => ipcRenderer.invoke('setup:clone-engine', { directory: dir }),
    scanModels: (dir) => ipcRenderer.invoke('setup:scan-models', dir),
    save: (values) => ipcRenderer.invoke('setup:save', { values }),
    openDownload: (kind) => ipcRenderer.invoke('setup:open-download', kind),
    onOpen: (cb) => ipcRenderer.on('setup:open', () => cb()),
  },

  pickWorkspace: () => ipcRenderer.invoke('workspace:pick'),
  useWorkspace: (dir) => ipcRenderer.invoke('workspace:use', dir),
  workspaceSnapshot: (dir) => ipcRenderer.invoke('workspace:snapshot', dir),

  listModels: () => ipcRenderer.invoke('models:list'),
  engine: {
    describe: () => ipcRenderer.invoke('engine:describe'),
    start: (opts) => ipcRenderer.invoke('engine:start', opts),
    stop: () => ipcRenderer.invoke('engine:stop'),
    cleanupOrphans: () => ipcRenderer.invoke('engine:cleanupOrphans'),
    stats: () => ipcRenderer.invoke('engine:stats'),
    log: () => ipcRenderer.invoke('engine:log'),
    doctor: (modelDir) => ipcRenderer.invoke('engine:doctor', modelDir),
  },
  files: {
    read: (rel) => ipcRenderer.invoke('file:read', rel),
    tree: () => ipcRenderer.invoke('file:tree'),
    reveal: (rel) => ipcRenderer.invoke('shell:reveal', rel),
  },
  npu: {
    status: () => ipcRenderer.invoke('npu:status'),
    probe: () => ipcRenderer.invoke('npu:probe'),
    index: (device) => ipcRenderer.invoke('npu:index', device),
    ensure: (options) => ipcRenderer.invoke('npu:ensure', options),
    fetchModel: () => ipcRenderer.invoke('npu:model'),
  },
  outlets: {
    status: (refresh) => ipcRenderer.invoke('outlets:status', { refresh: Boolean(refresh) }),
  },
  agent: {
    send: (payload) => ipcRenderer.invoke('agent:send', payload),
    stop: () => ipcRenderer.invoke('agent:stop'),
    approve: (id, approved) => ipcRenderer.invoke('approval:reply', { id, approved }),
    onEvent: (cb) => ipcRenderer.on('agent:event', (_e, data) => cb(data)),
    onApproval: (cb) => ipcRenderer.on('approval:request', (_e, data) => cb(data)),
  },
  stt: {
    transcribe: (audio, type) => ipcRenderer.invoke('stt:transcribe', { audio, type }),
  },
  updates: {
    check: () => ipcRenderer.invoke('update:check'),
    apply: () => ipcRenderer.invoke('update:apps'),
    open: (url) => ipcRenderer.invoke('update:open', url),
    onInfo: (cb) => ipcRenderer.on('update:info', (_e, data) => cb(data)),
  },
});
