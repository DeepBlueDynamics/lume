import React, { useMemo, useState, useEffect, useRef } from "react";
import { Canvas } from "@react-three/fiber";
import { OrbitControls, Line, Html, Billboard, Text } from "@react-three/drei";
import * as THREE from "three";
import { qHsl, OVERLAP, nodeColors, colorOfNode } from "./colors.js";

const fmtWeight = (n) => (Math.abs(n) >= 100 ? n.toFixed(0) : n.toFixed(2));

const UP = new THREE.Vector3(0, 1, 0);
const _v = new THREE.Vector3();
const _q = new THREE.Quaternion();

// One white radial-gradient texture, tinted per-halo via the material colour.
function radialTexture() {
  if (typeof document === "undefined") return null;
  const s = 128, c = document.createElement("canvas");
  c.width = c.height = s;
  const ctx = c.getContext("2d");
  const g = ctx.createRadialGradient(s / 2, s / 2, 0, s / 2, s / 2, s / 2);
  g.addColorStop(0, "rgba(255,255,255,0.95)");
  g.addColorStop(0.35, "rgba(255,255,255,0.4)");
  g.addColorStop(1, "rgba(255,255,255,0)");
  ctx.fillStyle = g;
  ctx.fillRect(0, 0, s, s);
  return new THREE.CanvasTexture(c);
}
const HALO_TEX = radialTexture();

function Halo({ pos, r, color, k = 1 }) {
  return (
    <Billboard position={pos}>
      <mesh>
        <planeGeometry args={[r * 6, r * 6]} />
        <meshBasicMaterial map={HALO_TEX} color={color} transparent opacity={0.75 * k}
          blending={THREE.AdditiveBlending} depthWrite={false} />
      </mesh>
    </Billboard>
  );
}

function Node({ node, color, halo, accScale, warp, onHover }) {
  const pos = node.pos;
  const isQ = node.is_query;
  const r = node.r ?? (isQ ? 0.3 : 0.2);

  // Warp the orb to show motion through the vector space: stretch into an
  // ellipsoid along velocity (with an acceleration pulse).
  const { quat, scale } = useMemo(() => {
    const vel = node.vel || [0, 0, 0];
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
      {halo && <Halo pos={pos} r={r} color={halo} k={isQ ? 1 : 0.85} />}
      <mesh
        position={pos}
        quaternion={isQ ? [0, 0, 0, 1] : quat}
        scale={isQ ? [1, 1, 1] : scale}
        onPointerOver={(e) => { e.stopPropagation(); onHover(node); }}
        onPointerOut={() => onHover(null)}
      >
        <sphereGeometry args={[r, 32, 32]} />
        <meshPhysicalMaterial color={color} emissive={color}
          emissiveIntensity={isQ ? 0.85 : 0.18 + 0.5 * Math.max(0, node.cos_q)}
          roughness={0.32} metalness={0.25} clearcoat={0.6} clearcoatRoughness={0.3} />
      </mesh>

      {accEnd && !isQ && (
        <Line points={[pos, accEnd]} color={color} lineWidth={node.approach_acc < 0 ? 2.4 : 1.1} />
      )}

      <Billboard position={[pos[0], pos[1] + r + 0.14, pos[2]]}>
        <Text fontSize={isQ ? 0.18 : 0.13} color={isQ ? "#ffffff" : color}
          anchorX="center" anchorY="bottom" outlineWidth={0.01} outlineColor="#05060a">
          {isQ ? "◆" : fmtWeight(node.score)}
        </Text>
      </Billboard>
    </group>
  );
}

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
    <OrbitControls ref={controls} makeDefault enablePan enableDamping dampingFactor={0.08}
      screenSpacePanning
      mouseButtons={{ LEFT: THREE.MOUSE.ROTATE, MIDDLE: THREE.MOUSE.DOLLY, RIGHT: THREE.MOUSE.PAN }} />
  );
}

function Tooltip({ node, color }) {
  if (!node) return null;
  const acc = node.approach_acc < 0 ? "#36d399" : "#ff5b6e";
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
          <span style={{ color: acc }}>d̈ {node.approach_acc >= 0 ? "+" : ""}{node.approach_acc.toFixed(3)}</span>
        </div>
        {node.members && node.members.length > 1 && (
          <div style={{ color: OVERLAP, marginBottom: 3 }}>★ overlap · queries {node.members.map((q) => q + 1).join(" + ")}</div>
        )}
        <div style={{ color: "#cfd6ee" }}>{node.text || node.label}</div>
      </div>
    </Html>
  );
}

export default function VectorField({ nodes, accScale, warp, queryCount, hoveredId, onHover }) {
  const multi = (queryCount || 1) > 1;

  // Shared colour logic (see colors.js) so list + orbs match.
  const colorById = useMemo(() => nodeColors(nodes, multi), [nodes, multi]);
  const colorOf = (nd) => colorOfNode(nd, colorById, multi);
  const haloOf = (nd) => {
    if (nd.is_query) return multi ? qHsl(nd.query_index, 66) : "#9fb4ff";
    if (nd.members && nd.members.length > 1) return OVERLAP;
    if (nd.id === hoveredId) return "#ffffff"; // highlight the hovered result
    return null;
  };
  const hoveredLive = hoveredId != null ? nodes.find((n) => n.id === hoveredId) : null;

  return (
    <Canvas camera={{ position: [0, 2, 16], fov: 50 }} dpr={[1, 2]}>
      <color attach="background" args={["#05060a"]} />
      <ambientLight intensity={0.4} />
      <directionalLight position={[6, 10, 8]} intensity={2.6} />
      <directionalLight position={[-8, 4, -6]} intensity={0.8} color="#6f8cff" />
      <pointLight position={[-10, -6, -8]} intensity={50} color="#3344ff" />
      <gridHelper args={[50, 50, "#10131f", "#0a0c14"]} position={[0, -7, 0]} />

      {nodes.map((n) => (
        <Node key={n.id} node={n} color={colorOf(n)} halo={haloOf(n)} accScale={accScale} warp={warp}
          onHover={(node) => onHover(node ? node.id : null)} />
      ))}
      <Tooltip node={hoveredLive} color={hoveredLive ? colorOf(hoveredLive) : "#888"} />

      <CtrlPanControls />
    </Canvas>
  );
}
