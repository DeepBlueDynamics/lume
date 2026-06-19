import React, { useMemo } from "react";
import { Canvas } from "@react-three/fiber";
import { OrbitControls, Line, Html, Billboard, Text } from "@react-three/drei";
import * as THREE from "three";

const CLUSTER_COLORS = [
  "#2b5cff", "#36d399", "#ffb13b", "#ff5b6e", "#7c4dff",
  "#22d3ee", "#f472b6", "#a3e635", "#fb923c", "#e879f9",
];

// Acceleration toward the query → green; away → red. (approach_acc is d̈ of the
// cosine *distance*, so negative = distance shrinking = moving toward query.)
function accColor(approachAcc) {
  return approachAcc < 0 ? "#36d399" : "#ff5b6e";
}

function Node({ node, accScale, showLabel }) {
  const pos = node.pos;
  const isQ = node.is_query;
  const color = isQ ? "#ffffff" : CLUSTER_COLORS[node.cluster % CLUSTER_COLORS.length];
  // closeness to query drives size + glow for candidates
  const r = isQ ? 0.42 : 0.16 + 0.28 * Math.max(0, node.cos_q);

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
      <mesh position={pos}>
        <sphereGeometry args={[r, 24, 24]} />
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

      {(showLabel || isQ) && (
        <Billboard position={[pos[0], pos[1] + r + 0.25, pos[2]]}>
          <Text fontSize={isQ ? 0.32 : 0.22} color={isQ ? "#ffffff" : "#aab2cc"}
            anchorX="center" anchorY="bottom" outlineWidth={0.012} outlineColor="#05060a">
            {node.label || `#${node.id}`}
          </Text>
        </Billboard>
      )}
    </group>
  );
}

export default function VectorField({ nodes, accScale, showLabels }) {
  return (
    <Canvas camera={{ position: [0, 2, 15], fov: 50 }} dpr={[1, 2]}>
      <color attach="background" args={["#05060a"]} />
      <ambientLight intensity={0.5} />
      <pointLight position={[10, 10, 10]} intensity={120} />
      <pointLight position={[-10, -6, -8]} intensity={40} color="#3344ff" />
      <gridHelper args={[40, 40, "#10131f", "#0a0c14"]} position={[0, -6, 0]} />

      {nodes.map((n) => (
        <Node key={n.id} node={n} accScale={accScale} showLabel={showLabels} />
      ))}

      <OrbitControls enableDamping dampingFactor={0.08} />
    </Canvas>
  );
}
