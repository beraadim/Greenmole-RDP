import { useState, useEffect } from "react";
import { Routes, Route, useNavigate } from "react-router-dom";
import Sidebar from "./components/Sidebar";
import Dashboard from "./pages/Dashboard";
import Settings from "./pages/Settings";
import Permissions from "./pages/Permissions";
import Logs from "./pages/Logs";
import Statistics from "./pages/Statistics";
import About from "./pages/About";
import Popover from "./pages/Popover";
import { api } from "./lib/ipc";
import { ThemeProvider } from "./contexts/ThemeContext";

function MainLayout() {
  const [initialized, setInitialized] = useState(false);
  const navigate = useNavigate();

  useEffect(() => {
    // Check permissions only while the service is running.
    // When the CLI is stopped, permission status is unavailable; open the dashboard.
    api
      .getServerStatus()
      .then((status) => {
        if (!status.running) {
          // The service is stopped; skip the permission check.
          setInitialized(true);
          return;
        }
        // The service is running; check its permissions.
        return api.getPermissions().then((perms) => {
          if (!perms.screen_capture || !perms.accessibility) {
            navigate("/permissions", { state: { firstLaunch: true } });
          }
          setInitialized(true);
        });
      })
      .catch(() => setInitialized(true));
  }, [navigate]);

  if (!initialized) {
    return (
      <div className="flex h-screen w-screen items-center justify-center bg-bg">
        <span className="text-sm text-text-muted">Loading...</span>
      </div>
    );
  }

  return (
    <div className="flex h-screen w-screen overflow-hidden">
      <Sidebar />
      <div className="flex flex-1 flex-col overflow-hidden">
        <main className="flex-1 overflow-auto bg-bg p-6">
          <Routes>
            <Route path="/" element={<Dashboard />} />
            <Route path="/settings" element={<Settings />} />
            <Route path="/permissions" element={<Permissions />} />
            <Route path="/logs" element={<Logs />} />
            <Route path="/statistics" element={<Statistics />} />
            <Route path="/about" element={<About />} />
          </Routes>
        </main>
      </div>
    </div>
  );
}

function App() {
  return (
    <ThemeProvider>
      <Routes>
        <Route path="/popover" element={<Popover />} />
        <Route path="/*" element={<MainLayout />} />
      </Routes>
    </ThemeProvider>
  );
}

export default App;
