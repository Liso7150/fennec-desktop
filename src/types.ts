export type Role = 'user' | 'assistant';

export interface ToolCall {
  id: string;
  name: string;
  args: Record<string, unknown>;
  phase: 'pending' | 'running' | 'done' | 'error';
  output?: string;
  message?: string;
}

export interface ChatMessage {
  id: string;
  role: Role;
  content: string;
  reasoning?: string;
  tools: ToolCall[];
  streaming?: boolean;
  error?: string;
  ms?: number;
  image?: { name: string; dataUrl: string };
}

export interface Todo {
  task: string;
  status: 'pending' | 'in_progress' | 'done';
}

export interface ModelInfo {
  dir: string;
  name: string;
  family: string;
  layers: number;
  experts: number;
  sizeGb: number;
  shards: number;
  gguf: boolean;
  container: boolean;
  ready: boolean;
  selected?: boolean;
}

export interface Plan {
  [key: string]: unknown;
}

export interface BackendState {
  runtime: string | null;
  python: string;
  port: number;
  model: string | null;
  modelId?: string | null;
  apiKey?: string;
  state: 'stopped' | 'starting' | 'ready' | 'failed';
  lastError: string | null;
  plan: Plan | null;
  uptimeMs: number;
  logPath: string | null;
}

export interface Settings {
  workspace: string;
  modelDir: string;
  port: number;
  kvSlots: number;
  temperature: number;
  topP: number;
  maxTokens: number;
  autoApproveWrites: boolean;
  autoApproveCommands: boolean;
  recentWorkspaces: string[];
  devicePolicy: string;
  npuDevice: string;
  npuEnabled: boolean;
}

export interface DeviceProbe {
  device: string;
  name?: string;
  status: 'ok' | 'absent' | 'error';
  compile_ms?: number;
  infer_ms?: number;
  error?: string;
}

export interface ProbeResult {
  ok: boolean;
  installed?: boolean;
  version?: string;
  available?: string[];
  devices?: DeviceProbe[];
  error?: string;
}

export interface AcceleratorStatus {
  python: string;
  pythonChecked?: string[];
  deps?: Record<string, string | null> | null;
  missingDeps?: string[];
  packages?: string[];
  modelPackages?: string[];
  embedModel: string | null;
  embedTarget?: string | null;
  embedModelId?: string;
  embedIrRepo?: string;
  lastProbe: ProbeResult | null;
  note?: string;
}

export interface TreeNode {
  name: string;
  path: string;
  dir: boolean;
  children?: TreeNode[];
}

declare global {
  interface Window {
    colibri: {
      info: () => Promise<{
        version: string;
        settings: Settings;
        backend: BackendState;
        accelerator?: AcceleratorStatus;
        packaged: boolean;
      }>;
      saveSettings: (patch: Partial<Settings>) => Promise<Settings>;
      pickWorkspace: () => Promise<{ workspace: string; recentWorkspaces: string[]; snapshot: string | null } | null>;
      useWorkspace: (dir: string) => Promise<{ workspace: string; snapshot: string | null }>;
      workspaceSnapshot: (dir: string) => Promise<string | null>;
      listModels: () => Promise<ModelInfo[]>;
      engine: {
        describe: () => Promise<BackendState>;
        start: (opts: { modelDir: string; port: number; kvSlots: number }) => Promise<{
          ok: boolean;
          backend: BackendState;
          error?: string;
          log?: string;
          settings?: Settings;
        }>;
        stop: () => Promise<BackendState>;
        stats: () => Promise<{ metrics: Record<string, number>; experts: unknown; profile: unknown }>;
        log: () => Promise<string>;
        doctor: (modelDir: string) => Promise<{ ok: boolean; report?: unknown; raw?: string; message?: string }>;
      };
      files: {
        read: (rel: string) => Promise<{ path: string; content: string }>;
        tree: () => Promise<TreeNode[]>;
        reveal: (rel: string) => Promise<void>;
      };
      npu: {
        status: () => Promise<AcceleratorStatus>;
        probe: () => Promise<ProbeResult>;
        index: (device: string) => Promise<{
          ok: boolean;
          device?: string;
          files?: number;
          chunks?: number;
          device_ms?: number;
          passes?: number;
          mode?: string;
          tokenizer?: boolean;
          embedding_shape?: number[];
          index?: string;
          error?: string | null;
        }>;
        ensure: (options?: { includeModelTooling?: boolean }) => Promise<{
          ok: boolean;
          code?: number;
          output?: string;
          python?: string;
          mode?: string;
          packages?: string[];
          ready?: boolean;
          error?: string;
        }>;
        fetchModel: () => Promise<{
          ok: boolean;
          repo?: string;
          dest?: string;
          model?: string | null;
          error?: string;
        }>;
      };
      agent: {
        send: (payload: { messages: ChatPayloadMessage[]; extraContext?: string }) => Promise<{
          ok: boolean;
          cancelled?: boolean;
          error?: string;
        }>;
        stop: () => Promise<boolean>;
        approve: (id: string, approved: boolean) => Promise<boolean>;
        onEvent: (cb: (event: AgentEvent) => void) => void;
        onApproval: (cb: (req: { id: string; name: string; args: Record<string, unknown> }) => void) => void;
      };
      stt: {
        transcribe: (audio: ArrayBuffer, type: string) => Promise<{
          ok: boolean;
          text?: string;
          language?: string;
          duration?: number;
          error?: string;
        }>;
      };
      updates: {
        check: () => Promise<UpdateInfo & { ok: boolean }>;
        apply: () => Promise<{
          ok: boolean;
          error?: string;
          results: { repo: string; ok: boolean; output: string }[];
        }>;
        open: (url: string) => Promise<boolean>;
        onInfo: (cb: (info: UpdateInfo) => void) => void;
      };
    };
  }
}

export type ContentPart =
  | { type: 'text'; text: string }
  | { type: 'image_url'; image_url: { url: string } };

export interface ChatPayloadMessage {
  role: string;
  content: string | ContentPart[];
}

export interface UpdateInfo {
  ok?: boolean;
  current?: string;
  latest?: string;
  upToDate?: boolean;
  available?: boolean;
  url?: string;
  name?: string;
  notes?: string;
  published?: string | null;
  checkedAt?: number | null;
  error?: string;
}

export type AgentEvent =
  | { type: 'step'; step: number }
  | { type: 'assistant'; text: string }
  | { type: 'reasoning'; text: string }
  | { type: 'usage'; promptTokens: number; completionTokens: number; tokensPerSecond: number }
  | { type: 'tool'; phase: 'start' | 'end' | 'error'; id: string; name: string; args?: Record<string, unknown>; output?: string; message?: string }
  | { type: 'todos'; todos: Todo[] }
  | { type: 'done'; text: string; ms: number }
  | { type: 'error'; message: string };
