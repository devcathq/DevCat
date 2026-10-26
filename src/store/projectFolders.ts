// Each focused project remembers its own folders (AI Workspace folder, Git scan folder);
// without focus the global ones are used. Follows this window's project focus. A folder used as the
// AI Workspace folder while a project is focused is that project's (see folderProjects).
import { useWorkspace } from "../features/claude/sidebar/workspace";
import { useGitScan } from "../features/git/scan";
import { useConnections } from "./connections";
import { useFolderProjects } from "./folderProjects";

function apply(projectId: string | null) {
  useWorkspace.getState().setProject(projectId);
  useGitScan.getState().setProject(projectId);
}

apply(useConnections.getState().focusProjectId);
useConnections.subscribe((s, prev) => {
  if (s.focusProjectId !== prev.focusProjectId) apply(s.focusProjectId);
});

function own({ project, cwd }: { project: string | null; cwd: string | null }) {
  if (project && cwd) useFolderProjects.getState().add(cwd, project);
}
own(useWorkspace.getState());
useWorkspace.subscribe((s, prev) => {
  if (s.cwd !== prev.cwd || s.project !== prev.project) own(s);
});
