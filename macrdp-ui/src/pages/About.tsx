import BrandLogo from "../components/BrandLogo";

function About() {
  return (
    <div className="flex h-full flex-col items-center justify-center p-4">
      <div className="w-full max-w-sm rounded-[10px] bg-card p-6 text-center">
        {/* App icon */}
        <BrandLogo className="mx-auto mb-3 h-20 w-20" alt="Greenmole mole mascot" />

        {/* App name */}
        <div className="text-lg font-semibold text-text">Greenmole RDP</div>

        {/* Version */}
        <div className="mt-0.5 text-xs text-text-muted">Version {import.meta.env.VITE_APP_VERSION}</div>
        <div className="mt-0.5 text-xs text-text-muted">Build {import.meta.env.VITE_BUILD_REVISION}</div>

        {/* Separator */}
        <div className="my-3 border-t border-border" />

        {/* Tech stack */}
        <div className="text-[11px] text-text-muted">
          IronRDP &middot; OpenH264 &middot; ScreenCaptureKit
        </div>

        {/* Links row */}
        <div className="mt-3 flex items-center justify-center gap-3">
          <a
            href="https://github.com/beraadim/macronrdp"
            target="_blank"
            rel="noopener noreferrer"
            className="text-xs text-accent hover:underline"
          >
            GitHub
          </a>
          <span className="text-xs text-text-muted">GPLv3 License</span>
          <a
            href="https://github.com/beraadim/macronrdp/releases"
            target="_blank"
            rel="noopener noreferrer"
            className="text-xs text-accent hover:underline"
          >
            Releases
          </a>
        </div>


      </div>
    </div>
  );
}

export default About;
