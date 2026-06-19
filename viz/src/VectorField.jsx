import React, { useMemo, useState, useEffect, useRef } from "react";
import { Canvas } from "@react-three/fiber";
import { OrbitControls, Line, Html, Billboard, Text } from "@react-three/drei";
import * as THREE from "three";

const fmtWeight = (n) => (Math.abs(n) >= 100 ? n.toFixed(0) : n.toFixed(2));
const hueColor = (hue, light = 58) => `hsl(${hue}, 85%, ${light}%)`;

const UP = new THREE.Vector3(0, 1, 0);
const _v = new THREE.Vector3();
const _q = new THREE.Quaternion();

function Node({ node, color, accScale, warp, onHover }) {
  const pos = node.pos;
  const isQ = node.is_query;
  const r = node.r ?? (isQ ? 0.3 : 0.2); // radius assigned by the layout/physics pass

  // Warp the orb to show its motion through the vector space: stretch into an
  // ellipsoid along the velocity direction (volume roughly preserved), so a
  // fast-moving / strongly-warped vector visibly elongates toward where it's
  // heading. Acceleration adds an extra pulse to the stretch.
  const { quat, scale } = useMemo(() => {
    const vel = node.vel;
    const speed = Math.hypot(vel[0], vel[1], vel[2]);
    const accMag = Math.hypot(node.acc[0], node.acc[1], node.acc[2]);
    const s = Math.min(1.6, (speed + 0.5 * accMag) * warp);
    let quat = [0, 0, 0, 1];
    if (speed > 1e-7) {
      _v.set(vel[0], vel[1], vel[2]).normalize();
      _q.setFromUnitVectors(UP, _v);
      quat = [_q.x, _q.y, _q.z, _q.w];
    }
    return { quat, scale: [1 - 0.35 * s, 1 + s, 1 - 0.35 * s] };
  }, [node.vel, node.acc, warp]);

  // Acceleration arrow along the 3D acceleration vector.
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
        quaternion={isQ ? [0, 0, 0, 1] : quat}
        scale={isQ ? [1, 1, 1] : scale}
        onPointerOver={(e) => { e.stopPropagation(); onHover(node); }}
        onPointerOut={() => onHover(null)}
      >
        <sphereGeometry args={[r, 32, 32]} />
        <meshPhysicalMaterial
          color={color}
          emissive={color}
          emissiveIntensity={isQ ? 0.85 : 0.18 + 0.5 * Math.max(0, node.cos_q)}
          roughness={0.32}
          metalness={0.25}
          clearcoat={0.6}
          clearcoatRoughness={0.3}
        />
      </mesh>

      {accEnd && !isQ && (
        <Line points={[pos, accEnd]} color={color} lineWidth={node.approach_acc < 0 ? 2.6 : 1.2} />
      )}

      <Billboard position={[pos[0], pos[1] + r + 0.18, pos[2]]}>
        <Text fontSize={isQ ? 0.26 : 0.2} color={isQ ? "#ffffff" : color}
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
    const onDown = (e) => { if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.PAN; };
    const onUp = (e) => { if (e.key === "Control" && controls.current) controls.current.mouseButtons.LEFT = THREE.MOUSE.ROTATE; };
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
      mouseButtons={{ LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN }}
    />
  );
}

function Tooltip({ node, color }) {
  if (!node) return null;
  const accColor = node.approach_acc < 0 ? "#36d399" : "#ff5b6e";
  return (
    <Html position={node.pos} distanceFactor={10} zIndexRange={[100, 0]} style={{ pointerEvents: "none" }}>
      <div style={{
        transform: "translate(14px, -50%)", width: 280, background: "rgba(10,12,20,0.94)",
        border: `1px solid ${color}`, borderRadius: 8, padding: "8px 10px", color: "#e6e8ef",
        fontSize: 11, lineHeight: 1.45, fontFamily: "ui-monospace, Menlo, Consolas, monospace",
      }}>
        <div style={{ display: "flex", justifyContent: "space-between", marginBottom: 4, color: "#9aa1b8" }}>
          <span>weight <b style={{ color: "#fff" }}>{node.is_query ? "—" : fmtWeight(node.score)}</b></span>
          <span>cos<sub>q</sub> <b style={{ color: "#fff" }}>{node.cos_q.toFixed(3)}</b></span>
          <span style={{ color: accColor }}>d̈ {node.approach_acc >= 0 ? "+" : ""}{node.approach_acc.toFixed(3)}</span>
        </div>
        <div style={{ color: "#cfd6ee" }}>{node.text || node.label}</div>
      </div>
    </Html>
  );
}

export default function VectorField({ nodes, accScale, warp }) {
  const [hovered, setHovered] = useState(null);

  // Rank candidates by cosine-to-query and spread them across the full spectrum,
  // so every result gets a distinct colour and relevance-neighbours sit next to
  // each other on the rainbow (warm = most related to the query).
  const colorById = useMemo(() => {
    const cands = nodes.filter((n) => !n.is_query).slice().sort((a, b) => b.cos_q - a.cos_q);
    const map = new Map();
    const n = Math.max(1, cands.length - 1);
    cands.forEach((node, i) => map.set(node.id, hueColor((i / n) * 300)));
    return map;
  }, [nodes]);

  const colorOf = (node) => (node.is_query ? "#ffffff" : colorById.get(node.id) || "#888");
  const hoveredLive = hovered != null ? nodes.find((n) => n.id === hovered.id) || hovered : null;

  return (
    <Canvas camera={{ position: [0, 2, 16], fov: 50 }} dpr={[1, 2]}>
      <color attach="background" args={["#05060a"]} />
      <ambientLight intensity={0.4} />
      <directionalLight position={[6, 10, 8]} intensity={2.6} />
      <directionalLight position={[-8, 4, -6]} intensity={0.8} color="#6f8cff" />
      <pointLight position={[-10, -6, -8]} intensity={50} color="#3344ff" />
      <gridHelper args={[50, 50, "#10131f", "#0a0c14"]} position={[0, -7, 0]} />

      {nodes.map((n) => (
        <Node key={n.id} node={n} color={colorOf(n)} accScale={accScale} warp={warp} onHover={setHovered} />
      ))}
      <Tooltip node={hoveredLive} color={hoveredLive ? colorOf(hoveredLive) : "#888"} />

      <CtrlPanControls />
    </Canvas>
  );
}
