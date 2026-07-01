import { Monitor, Sun, Moon } from "lucide-react";
import { useTheme } from "../contexts/ThemeContext";
import {
  Tooltip,
  TooltipContent,
  TooltipProvider,
  TooltipTrigger,
} from "./ui/tooltip";

const modes = ["light", "dark", "system"] as const;
const labels = { system: "Follow system", light: "Light mode", dark: "Dark mode" };
const icons = { system: Monitor, light: Sun, dark: Moon };

export default function ThemeToggle() {
  const { theme, setTheme } = useTheme();

  const cycle = () => {
    const idx = modes.indexOf(theme);
    setTheme(modes[(idx + 1) % modes.length]);
  };

  const Icon = icons[theme];

  return (
    <TooltipProvider>
      <Tooltip>
        <TooltipTrigger
          onClick={cycle}
          className="flex h-7 w-7 items-center justify-center rounded-md text-text-muted transition-colors hover:bg-accent/8 hover:text-text-secondary"
          render={<button />}
        >
          <Icon size={16} />
        </TooltipTrigger>
        <TooltipContent side="top">
          <p>{labels[theme]}</p>
        </TooltipContent>
      </Tooltip>
    </TooltipProvider>
  );
}
