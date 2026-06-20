import { agentBrowserRevalidationKey, installAgentBrowser } from "@agent-browser/sandbox/eve";
import { defineSandbox } from "eve/sandbox";
import { vercel } from "eve/sandbox/vercel";

export default defineSandbox({
  backend: vercel({ runtime: "node24", resources: { vcpus: 2 } }),
  revalidationKey: () => agentBrowserRevalidationKey({ installSystemDependencies: true }),
  async bootstrap({ use }) {
    const sandbox = await use();
    await installAgentBrowser(sandbox, { installSystemDependencies: true });
  },
});
