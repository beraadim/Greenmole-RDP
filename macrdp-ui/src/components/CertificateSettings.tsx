import { useEffect, useState } from "react";
import { api } from "../lib/ipc";
import type { TlsStatus, UiConfig } from "../lib/types";
import { Button } from "./ui/button";
import { Input } from "./ui/input";

export function CertificateSettings({ config, save }: {
  config: UiConfig;
  save: (key: keyof UiConfig, value: unknown) => Promise<boolean>;
}) {
  const [status, setStatus] = useState<TlsStatus | null>(null);
  const [certificate, setCertificate] = useState(config.tls_cert_path);
  const [key, setKey] = useState(config.tls_key_path);
  const [names, setNames] = useState(config.tls_server_names.join(", "));
  const [error, setError] = useState<string | null>(null);
  const refresh = () => api.getTlsStatus().then((value) => {
    setStatus(value); setError(null);
  }).catch((reason: unknown) => setError(String(reason)));

  useEffect(() => {
    setCertificate(config.tls_cert_path);
    setKey(config.tls_key_path);
    setNames(config.tls_server_names.join(", "));
    void refresh();
  }, [config.tls_cert_path, config.tls_key_path, config.tls_server_names]);

  const info = status?.certificate;
  const now = Date.now() / 1000;
  const expired = info && info.not_after_unix <= now;
  const notYetValid = info && info.not_before_unix > now;

  return (
    <section className="mt-4 space-y-3 border-t border-border pt-4 text-xs">
      <div className="font-semibold">TLS certificate</div>
      <p className="text-text-muted">
        Leave both paths empty to use an application-managed certificate. Custom
        certificates require both PEM files and are never replaced automatically.
        Changes take effect after restarting the service.
      </p>
      <label className="block space-y-1">
        <span>Certificate PEM path</span>
        <Input value={certificate} onChange={(event) => setCertificate(event.target.value)}
          onBlur={() => void save("tls_cert_path", certificate)} placeholder="Application-managed"
          className="h-8 text-xs" />
      </label>
      <label className="block space-y-1">
        <span>Private key PEM path</span>
        <Input value={key} onChange={(event) => setKey(event.target.value)}
          onBlur={() => void save("tls_key_path", key)} placeholder="Application-managed"
          className="h-8 text-xs" />
      </label>
      <label className="block space-y-1">
        <span>Additional server hostnames or IP addresses</span>
        <Input value={names} onChange={(event) => setNames(event.target.value)}
          onBlur={() => void save("tls_server_names", names.split(/[,\s]+/).filter(Boolean))}
          placeholder="mac-mini.local, 192.168.1.10" className="h-8 text-xs" />
      </label>
      <p className="text-text-muted">
        Use the names clients connect to. Renewing a managed certificate changes
        its fingerprint; clients may ask you to trust it again.
      </p>
      <div className="rounded-md border border-border p-3 space-y-2">
        <div className="flex items-center justify-between gap-2">
          <span>{status?.source === "running" ? "Active certificate" : "Configured certificate"}</span>
          <Button variant="ghost" size="sm" className="h-6 text-xs" onClick={() => void refresh()}>Refresh</Button>
        </div>
        {error || status?.error ? <p role="alert" className="text-red-600 dark:text-red-400">{error ?? status?.error}</p>
          : info ? <>
            <p className={expired || notYetValid ? "text-red-600 dark:text-red-400" : "text-text-muted"}>
              {expired ? "Expired" : notYetValid ? "Not yet valid" : "Expires"}: {info.expires_at}
            </p>
            <p className="break-all">Names: {info.server_names.join(", ")}</p>
            <p className="text-text-muted">SHA-256 fingerprint</p>
            <p className="break-all font-mono text-[10px]">{info.fingerprint_sha256}</p>
          </> : <p className="text-text-muted">A managed certificate will be created when the service starts.</p>}
      </div>
    </section>
  );
}
