import React, { useMemo, useState, useRef, useEffect } from "react";
import { Canvas, useThree } from "@react-three/fiber";
import { OrbitControls, Line, Html, Billboard, Text } from "@react-three/drei";
import * as THREE from "three";

const CLUSTER_COLORS = [
  "#2b5cff", "#36d399", "#ffb13b", "#ff5b6e", "#7c4dff",
  "#22d3ee", "#f472b6", "#a3e635", "#fb923c", "#e879f9",
];

// Acceleration toward the query → green; away → red. (approach_acc is d̈ of the
// cosine *distance*, so negative = distance shrinking = moving toward query.)
const accColor = (approachAcc) => (approachAcc < 0 ? "#36d399" : "#ff5b6e");
const fmtWeight = (n) => (Math.abs(n) >= 100 ? n.toFixed(0) : n.toFixed(2));

function Node({ node, accScale, onHover }) {
  const pos = node.pos;
  const isQ = node.is_query;
  const color = isQ ? "#ffffff" : CLUSTER_COLORS[node.cluster % CLUSTER_COLORS.length];
  const r = isQ ? 0.34 : 0.1 + 0.2 * Math.max(0, node.cos_q);

  // Acceleration arrow: from node position along its 3D acceleration vector.
  const accEnd = useMemo(() => {
    const a = node.acc;
    const mag = Math.hypot(a[0], a[1], a[2]);
    if (mag < 1e-9) return null;
    const len = Math.min(4, mag * accScale);
    const k = len / mag;
    return [pos[0] + a[0] * k, pos[1] + a[1] * k, pos[2] + a[2] * k];
  }, [node.acc, pos, accScale]);

  return (
    <group>
      <mesh
        position={pos}
        onPointerOver={(e) => { e.stopPropagation(); onHover(node); }}
        onPointerOut={() => onHover(null)}
      >
        <sphereGeometry args={[r, 20, 20]} />
        <meshStandardMaterial
          color={color}
          emissive={color}
          emissiveIntensity={isQ ? 0.9 : 0.25 + 0.6 * Math.max(0, node.cos_q)}
          roughness={0.35}
          metalness={0.1}
        />
      </mesh>

      {accEnd && !isQ && (
        <Line points={[pos, accEnd]} color={accColor(node.approach_acc)} lineWidth={2} />
      )}

      {/* Label = the retrieval weight (or ◆ for the query), short so it doesn't clutter. */}
      <Billboard position={[pos[0], pos[1] + r + 0.18, pos[2]]}>
        <Text fontSize={isQ ? 0.26 : 0.2} color={isQ ? "#ffffff" : "#cfd6ee"}
          anchorX="center" anchorY="bottom" outlineWidth={0.014} outlineColor="#05060a">
          {isQ ? "◆ query" : fmtWeight(node.score)}
        </Text>
      </Billboard>
    </group>
  );
}

// Hold Ctrl to switch left-drag from rotate to pan; release to restore.
function CtrlPanControls() {
  const controls = useRef();
  useEffect(() => {
    const onDown = (e) => {
      if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.PAN;
    };
    const onUp = (e) => {
      if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.ROTATE;
    };
    window.addEventListener("keydown", onDown);
    window.addEventListener("keyup", onUp);
    return () => { window.removeEventListener("keydown", onDown); window.removeEventListener("keyup", onUp); };
  }, []);
  return (
    <OrbitControls
      ref={controls}
      makeDefault
      enablePan
      enableDamping
      dampingFactor={0.08}
      screenSpacePanning
      panSpeed={1.0}
      // left=rotate (ctrl→pan), middle=dolly, right=pan
      mouseButtons={{ LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN }}
    />
  );
}

function Tooltip({ node }) {
  if (!node) return null;
  return (
    <Html position={node.pos} distanceFactor={10} zIndexRange={[100, 0]} style={{ pointerEvents: "none" }}>
      <div style={{
        transform: "translate(14px, -50%)", width: 280, background: "rgba(10,12,20,0.94)",
        border: "1px solid #2a3147", borderRadius: 8, padding: "8px 10px", color: "#e6e8ef",
        fontSize: 11, lineHeight: 1.45, fontFamily: "ui-monospace, Menlo, Consolas, monospace",
      }}>
        <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4, color: "#9aa1b8" }}>
          <span>weight <b style={{ color: "#fff" }}>{node.is_query ? "—" : fmtWeight(node.score)}</b></span>
          <span>cos<sub>q</sub> <b style={{ color: "#fff" }}>{node.cos_q.toFixed(3)}</b></span>
          <span style={{ color: accColor(node.approach_acc) }}>d̈ {node.approach_acc >= 0 ? "+" : ""}{node.approach_acc.toFixed(3)}</span>
        </div>
        <div style={{ color: "#cfd6ee" }}>{node.text || node.label}</div>
      </div>
    </Html>
  );
}

export default function VectorField({ nodes, accScale }) {
  const [hovered, setHovered] = useState(null);
  // Re-resolve the hovered node from the live frame so its tooltip follows it.
  const hoveredLive = hovered != null ? nodes.find((n) => n.id === hovered.id) || hovered : null;

  return (
    <Canvas camera={{ position: [0, 2, 16], fov: 50 }} dpr={[1, 2]}>
      <color attach="background" args={["#05060a"]} />
      <ambientLight intensity={0.5} />
      <pointLight position={[10, 10, 10]} intensity={120} />
      <pointLight position={[-10, -6, -8]} intensity={40} color="#3344ff" />
      <gridHelper args={[50, 50, "#10131f", "#0a0c14"]} position={[0, -7, 0]} />

      {nodes.map((n) => (
        <Node key={n.id} node={n} accScale={accScale} onHover={(node) => setHovered(node)} />
      ))}
      <Tooltip node={hoveredLive} />

      <CtrlPanControls />
    </Canvas>
  );
}
