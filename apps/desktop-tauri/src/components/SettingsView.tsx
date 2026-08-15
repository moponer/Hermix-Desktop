import { useState } from "react";
import { Settings, Cpu, UserCog } from "lucide-react";
import { PersonaView } from "./PersonaView";
import { ModelTab } from "./settings/ModelSettingsTab";
import { BackendTab } from "./settings/BackendSettingsTab";
import { AdvancedTab } from "./settings/AdvancedSettingsTab";

// ── Tabs ───────────────────────────────────────────

type Tab = "model" | "persona" | "backend" | "advanced";

export function SettingsView() {
  const [tab, setTab] = useState<Tab>("model");

  return (
    <div className="flex-1 flex flex-col overflow-hidden min-h-0">
      <div className="flex border-b border-border-primary px-4">
        <TabBtn active={tab === "model"} onClick={() => setTab("model")} icon={<Cpu size={14} />}>模型配置</TabBtn>
        <TabBtn active={tab === "persona"} onClick={() => setTab("persona")} icon={<UserCog size={14} />}>画像</TabBtn>
        <TabBtn active={tab === "backend"} onClick={() => setTab("backend")} icon={<Settings size={14} />}>后端设置</TabBtn>
        <TabBtn active={tab === "advanced"} onClick={() => setTab("advanced")} icon={<Settings size={14} />}>高级设置</TabBtn>
      </div>
      <div className="flex-1 overflow-y-auto min-h-0">
        {tab === "model" && <ModelTab />}
        {tab === "persona" && <PersonaView />}
        {tab === "backend" && <BackendTab />}
        {tab === "advanced" && <AdvancedTab />}
      </div>
    </div>
  );
}

function TabBtn({ active, onClick, icon, children }: { active: boolean; onClick: () => void; icon: React.ReactNode; children: React.ReactNode }) {
  return (
    <button onClick={onClick} className={`flex items-center gap-2 px-4 py-3 text-sm font-medium border-b-2 ${active ? "border-accent text-accent" : "border-transparent text-text-muted hover:text-text-primary"}`}>
      {icon} {children}
    </button>
  );
}
