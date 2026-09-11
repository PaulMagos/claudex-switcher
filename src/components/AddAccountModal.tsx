import { useEffect, useState } from "react";
import {
  describeFileSource,
  isTauriRuntime,
  openExternalUrl,
  pickAuthJsonFile,
  type FileSource,
} from "../lib/platform";
import type { Provider } from "../types";

interface AddAccountModalProps {
  isOpen: boolean;
  defaultProvider?: Provider;
  onClose: () => void;
  onImportFile: (source: FileSource, name: string) => Promise<void>;
  onStartOAuth: (name: string) => Promise<{ auth_url: string }>;
  onCompleteOAuth: () => Promise<unknown>;
  onCancelOAuth: () => Promise<void>;
  onStartClaudeOAuth: (name: string) => Promise<{ auth_url: string }>;
  onCompleteClaudeOAuth: (pastedCode: string) => Promise<unknown>;
  onCancelClaudeOAuth: () => Promise<void>;
  onImportClaudeCredentials: (source: FileSource, name: string) => Promise<void>;
  onAddClaudeApiKey: (name: string, apiKey: string) => Promise<void>;
}

type CodexTab = "oauth" | "import";
type ClaudeTab = "oauth" | "import" | "api_key";

export function AddAccountModal({
  isOpen,
  defaultProvider = "codex",
  onClose,
  onImportFile,
  onStartOAuth,
  onCompleteOAuth,
  onCancelOAuth,
  onStartClaudeOAuth,
  onCompleteClaudeOAuth,
  onCancelClaudeOAuth,
  onImportClaudeCredentials,
  onAddClaudeApiKey,
}: AddAccountModalProps) {
  const [provider, setProvider] = useState<Provider>(defaultProvider);
  const [codexTab, setCodexTab] = useState<CodexTab>("oauth");
  const [claudeTab, setClaudeTab] = useState<ClaudeTab>("oauth");
  const [name, setName] = useState("");
  const [fileSource, setFileSource] = useState<FileSource | null>(null);
  const [apiKey, setApiKey] = useState("");
  const [pastedCode, setPastedCode] = useState("");
  const [loading, setLoading] = useState(false);
  const [error, setError] = useState<string | null>(null);
  const [oauthPending, setOauthPending] = useState(false);
  const [authUrl, setAuthUrl] = useState<string>("");
  const [copied, setCopied] = useState<boolean>(false);
  const tauriRuntime = isTauriRuntime();
  const activeTab = provider === "codex" ? codexTab : claudeTab;
  const isPrimaryDisabled =
    loading || (activeTab === "oauth" && oauthPending && provider === "codex");

  const resetForm = () => {
    setName("");
    setFileSource(null);
    setApiKey("");
    setPastedCode("");
    setError(null);
    setLoading(false);
    setOauthPending(false);
    setAuthUrl("");
  };

  useEffect(() => {
    if (isOpen) setProvider(defaultProvider);
  }, [isOpen, defaultProvider]);

  const handleClose = () => {
    if (oauthPending) {
      if (provider === "codex") void onCancelOAuth();
      else void onCancelClaudeOAuth();
    }
    resetForm();
    onClose();
  };

  const switchProvider = (next: Provider) => {
    if (oauthPending) {
      if (provider === "codex") void onCancelOAuth();
      else void onCancelClaudeOAuth();
    }
    setProvider(next);
    setOauthPending(false);
    setLoading(false);
    setError(null);
    setAuthUrl("");
    setPastedCode("");
  };

  const handleOAuthLogin = async () => {
    try {
      setLoading(true);
      setError(null);
      const info = await onStartOAuth(name.trim());
      setAuthUrl(info.auth_url);
      setOauthPending(true);
      setLoading(false);

      // Wait for completion
      await onCompleteOAuth();
      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
      setOauthPending(false);
    }
  };

  const handleClaudeOAuthStart = async () => {
    try {
      setLoading(true);
      setError(null);
      const info = await onStartClaudeOAuth(name.trim());
      setAuthUrl(info.auth_url);
      setOauthPending(true);
      setLoading(false);
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
      setOauthPending(false);
    }
  };

  const handleClaudeOAuthComplete = async () => {
    if (!pastedCode.trim()) {
      setError("Paste the code Claude showed after you approved access.");
      return;
    }
    try {
      setLoading(true);
      setError(null);
      await onCompleteClaudeOAuth(pastedCode.trim());
      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
    }
  };

  const handleSelectFile = async () => {
    try {
      const selected = await pickAuthJsonFile();
      if (selected) setFileSource(selected);
    } catch (err) {
      console.error("Failed to open file dialog:", err);
    }
  };

  const handleImportFile = async () => {
    if (!fileSource) {
      setError("Please select a file to import");
      return;
    }

    try {
      setLoading(true);
      setError(null);
      if (provider === "codex") {
        await onImportFile(fileSource, name.trim());
      } else {
        await onImportClaudeCredentials(fileSource, name.trim());
      }
      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
    }
  };

  const handleAddClaudeApiKey = async () => {
    if (!apiKey.trim()) {
      setError("Please enter an Anthropic API key");
      return;
    }
    try {
      setLoading(true);
      setError(null);
      await onAddClaudeApiKey(name.trim(), apiKey.trim());
      handleClose();
    } catch (err) {
      setError(err instanceof Error ? err.message : String(err));
      setLoading(false);
    }
  };

  const handlePrimaryAction = () => {
    if (provider === "codex") {
      return codexTab === "oauth" ? handleOAuthLogin() : handleImportFile();
    }
    if (claudeTab === "oauth") {
      return oauthPending ? handleClaudeOAuthComplete() : handleClaudeOAuthStart();
    }
    if (claudeTab === "import") return handleImportFile();
    return handleAddClaudeApiKey();
  };

  const primaryLabel = () => {
    if (loading) return "Working...";
    if (provider === "codex") {
      return codexTab === "oauth" ? "Generate Login Link" : "Import";
    }
    if (claudeTab === "oauth") return oauthPending ? "I've pasted the code" : "Generate Login Link";
    if (claudeTab === "import") return "Import";
    return "Add Account";
  };

  if (!isOpen) return null;

  return (
    <div className="fixed inset-0 bg-black/40 flex items-center justify-center z-50">
      <div className="bg-white dark:bg-gray-900 border border-gray-200 dark:border-gray-700 rounded-2xl w-full max-w-md mx-4 shadow-xl">
        {/* Header */}
        <div className="flex items-center justify-between p-5 border-b border-gray-100 dark:border-gray-800">
          <h2 className="text-lg font-semibold text-gray-900 dark:text-gray-100">Add Account</h2>
          <button
            onClick={handleClose}
            className="text-gray-400 hover:text-gray-600 dark:hover:text-gray-300 transition-colors"
          >
            ✕
          </button>
        </div>

        {/* Provider toggle */}
        <div className="flex gap-2 p-4 pb-0">
          {(["codex", "claude"] as Provider[]).map((p) => (
            <button
              key={p}
              onClick={() => switchProvider(p)}
              className={`flex-1 px-3 py-2 text-sm font-medium rounded-lg border transition-colors ${
                provider === p
                  ? "bg-gray-900 dark:bg-gray-100 text-white dark:text-gray-900 border-gray-900 dark:border-gray-100"
                  : "bg-white dark:bg-gray-800 text-gray-600 dark:text-gray-300 border-gray-200 dark:border-gray-700 hover:bg-gray-50 dark:hover:bg-gray-700"
              }`}
            >
              {p === "codex" ? "Codex" : "Claude Code"}
            </button>
          ))}
        </div>

        {/* Tabs */}
        <div className="flex border-b border-gray-100 dark:border-gray-800 mt-4">
          {provider === "codex"
            ? (["oauth", "import"] as CodexTab[]).map((tab) => (
                <button
                  key={tab}
                  onClick={() => {
                    if (tab === "import" && oauthPending) {
                      void onCancelOAuth().catch((err) => {
                        console.error("Failed to cancel login:", err);
                      });
                      setOauthPending(false);
                      setLoading(false);
                    }
                    setCodexTab(tab);
                    setError(null);
                  }}
                  className={`flex-1 px-4 py-3 text-sm font-medium transition-colors ${
                    codexTab === tab
                      ? "text-gray-900 dark:text-gray-100 border-b-2 border-gray-900 dark:border-gray-100 -mb-px"
                      : "text-gray-400 dark:text-gray-500 hover:text-gray-600 dark:hover:text-gray-300"
                  }`}
                >
                  {tab === "oauth" ? "ChatGPT Login" : "Import File"}
                </button>
              ))
            : (["oauth", "import", "api_key"] as ClaudeTab[]).map((tab) => (
                <button
                  key={tab}
                  onClick={() => {
                    if (tab !== "oauth" && oauthPending) {
                      void onCancelClaudeOAuth().catch((err) => {
                        console.error("Failed to cancel Claude login:", err);
                      });
                      setOauthPending(false);
                      setLoading(false);
                    }
                    setClaudeTab(tab);
                    setError(null);
                  }}
                  className={`flex-1 px-3 py-3 text-sm font-medium transition-colors ${
                    claudeTab === tab
                      ? "text-gray-900 dark:text-gray-100 border-b-2 border-gray-900 dark:border-gray-100 -mb-px"
                      : "text-gray-400 dark:text-gray-500 hover:text-gray-600 dark:hover:text-gray-300"
                  }`}
                >
                  {tab === "oauth" ? "Claude Login" : tab === "import" ? "Import File" : "API Key"}
                </button>
              ))}
        </div>

        {/* Content */}
        <div className="p-5 space-y-4">
          {/* Account name is optional; the backend derives one when blank. */}
          <div>
            <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
              Account Name (optional)
            </label>
            <input
              type="text"
              value={name}
              onChange={(e) => setName(e.target.value)}
              placeholder="Leave blank to use email"
              className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-gray-400 dark:focus:border-gray-500 focus:ring-1 focus:ring-gray-400 dark:focus:ring-gray-500 transition-colors"
            />
          </div>

          {/* Codex OAuth */}
          {provider === "codex" && codexTab === "oauth" && (
            <div className="text-sm text-gray-500 dark:text-gray-400">
              {oauthPending ? (
                <div className="text-center py-4">
                  <div className="animate-spin h-8 w-8 border-2 border-gray-900 dark:border-gray-100 border-t-transparent rounded-full mx-auto mb-3"></div>
                  <p className="text-gray-700 dark:text-gray-300 font-medium mb-2">Waiting for browser login...</p>
                  <p className="text-xs text-gray-500 dark:text-gray-400 mb-4">
                    Please open the following link in your browser to proceed:
                  </p>
                  <AuthUrlBox authUrl={authUrl} copied={copied} setCopied={setCopied} setError={setError} />
                  {!tauriRuntime && (
                    <p className="text-xs text-amber-600">
                      OAuth login must finish on the same host machine because the callback
                      redirects to `localhost`.
                    </p>
                  )}
                </div>
              ) : (
                <p>
                  Click the button below to generate a login link.
                  You will need to open it in your browser to authenticate.
                </p>
              )}
            </div>
          )}

          {/* Codex import */}
          {provider === "codex" && codexTab === "import" && (
            <div>
              <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
                Select auth.json file
              </label>
              <div className="flex gap-2">
                <div className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-sm text-gray-600 dark:text-gray-300 truncate">
                  {describeFileSource(fileSource)}
                </div>
                <button
                  onClick={handleSelectFile}
                  className="px-4 py-2.5 bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 border border-gray-200 dark:border-gray-700 rounded-lg text-sm font-medium text-gray-700 dark:text-gray-200 transition-colors whitespace-nowrap"
                >
                  Browse...
                </button>
              </div>
              <p className="text-xs text-gray-400 dark:text-gray-500 mt-2">
                Import credentials from an existing Codex auth.json file
              </p>
            </div>
          )}

          {/* Claude OAuth */}
          {provider === "claude" && claudeTab === "oauth" && (
            <div className="text-sm text-gray-500 dark:text-gray-400 space-y-3">
              {oauthPending ? (
                <>
                  <p className="text-xs text-gray-500 dark:text-gray-400">
                    Open the link below, approve access, then paste the code Claude shows
                    (it looks like <code>code#state</code>) into the box.
                  </p>
                  <AuthUrlBox authUrl={authUrl} copied={copied} setCopied={setCopied} setError={setError} />
                  <input
                    type="text"
                    value={pastedCode}
                    onChange={(e) => setPastedCode(e.target.value)}
                    placeholder="Paste authorization code here"
                    className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-gray-400 dark:focus:border-gray-500 focus:ring-1 focus:ring-gray-400 dark:focus:ring-gray-500 transition-colors"
                  />
                </>
              ) : (
                <p>
                  Click the button below to generate a Claude Code login link. Claude Code's
                  login flow shows a code after you approve access instead of redirecting back
                  automatically — you'll paste it in here.
                </p>
              )}
            </div>
          )}

          {/* Claude import */}
          {provider === "claude" && claudeTab === "import" && (
            <div>
              <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
                Select .credentials.json file
              </label>
              <div className="flex gap-2">
                <div className="flex-1 px-4 py-2.5 bg-gray-50 dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-sm text-gray-600 dark:text-gray-300 truncate">
                  {describeFileSource(fileSource)}
                </div>
                <button
                  onClick={handleSelectFile}
                  className="px-4 py-2.5 bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 border border-gray-200 dark:border-gray-700 rounded-lg text-sm font-medium text-gray-700 dark:text-gray-200 transition-colors whitespace-nowrap"
                >
                  Browse...
                </button>
              </div>
              <p className="text-xs text-gray-400 dark:text-gray-500 mt-2">
                Import credentials from an existing Claude Code ~/.claude/.credentials.json file
              </p>
            </div>
          )}

          {/* Claude API key */}
          {provider === "claude" && claudeTab === "api_key" && (
            <div>
              <label className="block text-sm font-medium text-gray-700 dark:text-gray-300 mb-2">
                Anthropic API key
              </label>
              <input
                type="password"
                value={apiKey}
                onChange={(e) => setApiKey(e.target.value)}
                placeholder="sk-ant-..."
                className="w-full px-4 py-2.5 bg-white dark:bg-gray-800 border border-gray-200 dark:border-gray-700 rounded-lg text-gray-900 dark:text-gray-100 placeholder-gray-400 dark:placeholder-gray-500 focus:outline-none focus:border-gray-400 dark:focus:border-gray-500 focus:ring-1 focus:ring-gray-400 dark:focus:ring-gray-500 transition-colors"
              />
              <p className="text-xs text-gray-400 dark:text-gray-500 mt-2">
                Usage stats and rate-limit windows are not available for API key accounts.
              </p>
            </div>
          )}

          {/* Error */}
          {error && (
            <div className="p-3 bg-red-50 dark:bg-red-900/20 border border-red-200 dark:border-red-700 rounded-lg text-red-600 dark:text-red-300 text-sm">
              {error}
            </div>
          )}
        </div>

        {/* Footer */}
        <div className="flex gap-3 p-5 border-t border-gray-100 dark:border-gray-800">
          <button
            onClick={handleClose}
            className="flex-1 px-4 py-2.5 text-sm font-medium rounded-lg bg-gray-100 hover:bg-gray-200 dark:bg-gray-800 dark:hover:bg-gray-700 text-gray-700 dark:text-gray-200 transition-colors"
          >
            Cancel
          </button>
          <button
            onClick={() => void handlePrimaryAction()}
            disabled={isPrimaryDisabled}
            className="flex-1 px-4 py-2.5 text-sm font-medium rounded-lg bg-gray-900 hover:bg-gray-800 dark:bg-gray-100 dark:hover:bg-gray-200 text-white dark:text-gray-900 transition-colors disabled:opacity-50"
          >
            {primaryLabel()}
          </button>
        </div>
      </div>
    </div>
  );
}

function AuthUrlBox({
  authUrl,
  copied,
  setCopied,
  setError,
}: {
  authUrl: string;
  copied: boolean;
  setCopied: (value: boolean) => void;
  setError: (value: string | null) => void;
}) {
  return (
    <div className="flex items-center gap-2 mb-2 bg-gray-50 dark:bg-gray-800 p-2 rounded-lg border border-gray-200 dark:border-gray-700">
      <input
        type="text"
        readOnly
        value={authUrl}
        className="flex-1 bg-transparent border-none text-xs text-gray-600 dark:text-gray-300 focus:outline-none focus:ring-0 truncate"
      />
      <button
        onClick={() => {
          void navigator.clipboard
            .writeText(authUrl)
            .then(() => {
              setCopied(true);
              setTimeout(() => setCopied(false), 2000);
            })
            .catch(() => {
              setError("Clipboard unavailable. Copy the link manually.");
            });
        }}
        className={`px-3 py-1.5 border rounded text-xs font-medium transition-colors shrink-0
          ${copied
            ? "bg-green-50 dark:bg-green-900/30 border-green-200 dark:border-green-700 text-green-700 dark:text-green-300"
            : "bg-white dark:bg-gray-900 border-gray-200 dark:border-gray-700 text-gray-700 dark:text-gray-200 hover:bg-gray-50 dark:hover:bg-gray-800"
          }`}
      >
        {copied ? "Copied!" : "Copy"}
      </button>
      <button
        onClick={() => {
          void openExternalUrl(authUrl);
        }}
        className="px-3 py-1.5 bg-gray-900 hover:bg-gray-800 dark:bg-gray-100 dark:hover:bg-gray-200 border border-gray-900 dark:border-gray-100 rounded text-xs font-medium text-white dark:text-gray-900 transition-colors shrink-0"
      >
        Open
      </button>
    </div>
  );
}
