// The "Why jrs" page: static prose, so only the shared chrome is wired up.

import { $$, highlight, setupCopy, stampVersion } from "./shared";

stampVersion();
for (const pre of $$<HTMLPreElement>("pre[data-lang]")) highlight(pre);
setupCopy();
