import { useEffect, useState } from "react";
import { getChannelStatus, type ChannelStatusItem } from "../api";

/**
 * IM 通道连接状态（含机器人名）。
 *
 * 30s 轮询：端点是本地 HTTP，代价可忽略；连接状态点要"活着"，
 * 只在挂载时抓一次会停在过期状态上。
 */
export function useChannelsStatus(): ChannelStatusItem[] | null {
  const [status, setStatus] = useState<ChannelStatusItem[] | null>(null);
  useEffect(() => {
    let ok = true;
    const load = () => {
      getChannelStatus().then((r) => {
        if (ok) setStatus(r ? r.platforms : null);
      });
    };
    load();
    const t = window.setInterval(load, 30_000);
    return () => {
      ok = false;
      window.clearInterval(t);
    };
  }, []);
  return status;
}
