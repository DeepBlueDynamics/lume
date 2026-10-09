# Signal K description bundle

signalk_paths.json extracts descriptions from SignalK/specification 1.8.4, commit fb628fb4ee569149fd3810b271d0f987428b4703:
https://github.com/SignalK/specification/tree/fb628fb4ee569149fd3810b271d0f987428b4703/schemas

Upstream authors: the Signal K project contributors. The extracted description bundle is distributed under the upstream Creative Commons Attribution-ShareAlike 2.0 license; see LICENSE in this directory. The Rust implementation is separate from this licensed schema data.

Extraction traverses properties, patternProperties (represented as *), allOf/anyOf/oneOf and local/shared definition references across navigation, environment, electrical, propulsion, tanks, steering, performance, design, communication and sails. Scalar value wrappers are flattened; metadata/source/timestamp wrappers are excluded. Parent descriptions provide context for dynamic battery, engine and tank instances. 488 metadata patterns are bundled. The resolver indexes only fields present in the store catalog, never unreported spec-only paths. Custom fields use their path tokens. No network is needed at runtime.

The golden phrases are separately authored in tests/golden/resolve.json; none are consumed by the implementation.
