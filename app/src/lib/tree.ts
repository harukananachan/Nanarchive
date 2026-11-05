import type { ArchiveView } from "./api";

/** One file or folder inside an archive. Node 0 is the archive root. */
export type Node = {
  name: string;
  path: string;
  dir: boolean;
  size: number;
  mtime: number;
  parent: number;
  kids: number[];
  lower: string;
};

/** Builds a folder tree from the flat, column-wise file list. */
export function buildTree(a: ArchiveView): Node[] {
  const nodes: Node[] = [{ name: "", path: "", dir: true, size: 0, mtime: -1, parent: -1, kids: [], lower: "" }];
  const byPath = new Map<string, number>([["", 0]]);
  const ensureDir = (path: string): number => {
    const hit = byPath.get(path);
    if (hit !== undefined) return hit;
    const cut = path.lastIndexOf("/");
    const parent = ensureDir(cut < 0 ? "" : path.slice(0, cut));
    const name = path.slice(cut + 1);
    const id = nodes.length;
    nodes.push({ name, path, dir: true, size: 0, mtime: -1, parent, kids: [], lower: name.toLowerCase() });
    nodes[parent].kids.push(id);
    byPath.set(path, id);
    return id;
  };
  for (let i = 0; i < a.paths.length; i++) {
    const path = a.paths[i];
    if (a.is_dir[i]) {
      nodes[ensureDir(path)].mtime = a.mtimes[i];
      continue;
    }
    const cut = path.lastIndexOf("/");
    const parent = ensureDir(cut < 0 ? "" : path.slice(0, cut));
    const name = path.slice(cut + 1);
    const id = nodes.length;
    nodes.push({ name, path, dir: false, size: a.sizes[i], mtime: a.mtimes[i], parent, kids: [], lower: name.toLowerCase() });
    nodes[parent].kids.push(id);
  }
  // Parents always come before their children, so one backwards pass sums folder sizes.
  for (let i = nodes.length - 1; i > 0; i--) nodes[nodes[i].parent].size += nodes[i].size;
  return nodes;
}
