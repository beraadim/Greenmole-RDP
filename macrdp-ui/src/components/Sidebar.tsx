import { useState, useEffect } from "react";
import { Link, useLocation } from "react-router-dom";
import {
  Monitor,
  Settings,
  Shield,
  FileText,
  Activity,
  Info,
} from "lucide-react";
import { api } from "../lib/ipc";
import type { PermissionStatus } from "../lib/types";
import ThemeToggle from "./ThemeToggle";

const navGroups = [
  {
    label: "Service",
    items: [{ path: "/", icon: Monitor, label: "Dashboard" }],
  },
  {
    label: "Configuration",
    items: [
      { path: "/settings", icon: Settings, label: "Settings" },
      { path: "/permissions", icon: Shield, label: "Permissions" },
    ],
  },
  {
    label: "Diagnostics",
    items: [
      { path: "/logs", icon: FileText, label: "Logs" },
      { path: "/statistics", icon: Activity, label: "Statistics" },
    ],
  },
];

function Sidebar() {
  const [perms, setPerms] = useState<PermissionStatus | null>(null);
  const location = useLocation();

  useEffect(() => {
    const check = () => api.getPermissions().then(setPerms).catch(() => {});
    check();
    const interval = setInterval(check, 5000);
    return () => clearInterval(interval);
  }, []);

  const hasPermissionIssue =
    perms !== null && (!perms.screen_capture || !perms.accessibility);

  const isActive = (path: string) =>
    path === "/" ? location.pathname === "/" : location.pathname.startsWith(path);

  return (
    <aside className="flex h-full w-[180px] flex-shrink-0 flex-col border-r border-border bg-sidebar">
      {/* Titlebar drag region */}
      <div
        className="h-7 flex-shrink-0"
        data-tauri-drag-region
        style={{ WebkitAppRegion: "drag" } as React.CSSProperties}
      />

      {/* Navigation */}
      <nav role="navigation" className="flex-1 overflow-y-auto px-3">
        {navGroups.map((group) => (
          <div key={group.label} className="mb-3">
            <div className="mb-1 px-2 text-[10px] font-semibold uppercase tracking-wider text-text-muted">
              {group.label}
            </div>
            <div className="space-y-0.5">
              {group.items.map(({ path, icon: Icon, label }) => {
                const active = isActive(path);
                return (
                  <Link
                    key={path}
                    to={path}
                    aria-current={active ? "page" : undefined}
                    className={`flex items-center gap-2 rounded-md px-2 py-1.5 text-xs font-medium transition-colors ${
                      active
                        ? "bg-accent/15 text-accent"
                        : "text-text-secondary hover:bg-accent/8"
                    }`}
                  >
                    <span className="relative flex-shrink-0">
                      <Icon size={16} />
                      {path === "/permissions" && hasPermissionIssue && (
                        <span className="absolute -right-1 -top-1 h-2 w-2 rounded-full bg-red-500" />
                      )}
                    </span>
                    <span>{label}</span>
                  </Link>
                );
              })}
            </div>
          </div>
        ))}
      </nav>

      {/* Bottom fixed area */}
      <div className="flex-shrink-0 border-t border-border px-3 py-2">
        <Link
          to="/about"
          aria-current={isActive("/about") ? "page" : undefined}
          className={`flex items-center gap-2 rounded-md px-2 py-1.5 text-xs font-medium transition-colors ${
            isActive("/about")
              ? "bg-accent/15 text-accent"
              : "text-text-secondary hover:bg-accent/8"
          }`}
        >
          <Info size={16} />
          <span>About</span>
        </Link>
        <div className="mt-2 flex items-center px-2">
          <ThemeToggle />
        </div>
      </div>
    </aside>
  );
}

export default Sidebar;
