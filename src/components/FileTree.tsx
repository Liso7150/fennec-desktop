import { useState } from 'react';
import type { TreeNode } from '../types';

function Row({ node, depth, onOpen }: { node: TreeNode; depth: number; onOpen: (path: string) => void }) {
  const [open, setOpen] = useState(depth < 2);
  return (
    <>
      <button
        className={`tree-row ${node.dir ? 'dir' : ''}`}
        style={{ paddingLeft: 8 + depth * 12 }}
        onClick={() => (node.dir ? setOpen((v) => !v) : onOpen(node.path))}
        title={node.path}
      >
        <span className="tree-glyph">{node.dir ? (open ? '▾' : '▸') : '·'}</span>
        <span className="tree-name">{node.name}</span>
      </button>
      {node.dir && open && node.children?.map((child) => (
        <Row key={child.path} node={child} depth={depth + 1} onOpen={onOpen} />
      ))}
    </>
  );
}

export default function FileTree({ nodes, onOpen }: { nodes: TreeNode[]; onOpen: (path: string) => void }) {
  if (!nodes.length) return <div className="muted pad">No folder open</div>;
  return (
    <div className="tree">
      {nodes.map((node) => (
        <Row key={node.path} node={node} depth={0} onOpen={onOpen} />
      ))}
    </div>
  );
}
