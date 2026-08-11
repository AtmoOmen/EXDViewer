#version 300 es

layout(location = 0) in vec3 a_position;
layout(location = 1) in vec3 a_normal;
layout(location = 2) in vec4 a_tangent;
layout(location = 3) in vec2 a_uv;
layout(location = 4) in vec4 a_color;
layout(location = 5) in uvec4 a_weights;
layout(location = 6) in uvec4 a_bones;

uniform mat4 u_view;
uniform mat4 u_projection;
/// The joint palette, one bone to four columns of three floats. Rows of `program::ROW` dwords, which
/// is the width whatever fills the texture writes it at.
uniform highp usampler2D u_joints;
uniform bool u_skinned;

out vec3 v_position;
out vec3 v_normal;
out vec4 v_tangent;
out vec2 v_uv;
out vec4 v_color;

const uint ROW = 1024u;
const uint JOINT = 12u;

vec3 column(uint at) {
	return uintBitsToFloat(uvec3(
		texelFetch(u_joints, ivec2(int(at % ROW), int(at / ROW)), 0).x,
		texelFetch(u_joints, ivec2(int((at + 1u) % ROW), int((at + 1u) / ROW)), 0).x,
		texelFetch(u_joints, ivec2(int((at + 2u) % ROW), int((at + 2u) / ROW)), 0).x));
}

mat4 joint(uint bone) {
	uint at = bone * JOINT;
	return mat4(
		vec4(column(at), 0.0),
		vec4(column(at + 3u), 0.0),
		vec4(column(at + 6u), 0.0),
		vec4(column(at + 9u), 1.0));
}

void main() {
	vec3 position = a_position;
	vec3 normal = a_normal;
	vec4 tangent = a_tangent * 2.0 - 1.0;

	if (u_skinned) {
		vec3 moved = vec3(0.0);
		vec3 turned = vec3(0.0);
		vec3 along = vec3(0.0);
		float total = 0.0;
		// Eight influences: the low byte of each pair carries the first four and the high byte the
		// second four, which is how the game's own shaders read them.
		for (int at = 0; at < 8; ++at) {
			uint shift = at < 4 ? 0u : 8u;
			float weight = float((a_weights[at & 3] >> shift) & 255u) / 255.0;
			if (weight == 0.0) {
				continue;
			}
			mat4 held = joint((a_bones[at & 3] >> shift) & 255u);
			moved += (held * vec4(a_position, 1.0)).xyz * weight;
			turned += mat3(held) * a_normal * weight;
			along += mat3(held) * tangent.xyz * weight;
			total += weight;
		}
		if (total > 0.0) {
			position = moved / total;
			normal = turned;
			tangent.xyz = along;
		}
	}

	v_position = position;
	v_normal = normal;
	v_tangent = tangent;
	v_uv = a_uv;
	v_color = a_color;
	gl_Position = u_projection * u_view * vec4(position, 1.0);
}
