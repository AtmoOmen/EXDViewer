#version 300 es

layout(location = 0) in vec2 a_corner;

uniform mat4 u_view_projection;
/// Where the quad is centered and how far it reaches, in the ground's own two axes.
uniform vec2 u_center;
uniform float u_extent;

out vec2 v_ground;
/// How far along the view the fragment stands, which is what the clip planes are stated in.
out float v_depth;

void main() {
	v_ground = u_center + a_corner * u_extent;
	gl_Position = u_view_projection * vec4(v_ground.x, 0.0, v_ground.y, 1.0);
	v_depth = gl_Position.w;
}
