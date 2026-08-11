//! The floor under the model: one quad at y=0, ruled in the fragment shader so its lines stay a
//! pixel wide at any zoom and no geometry has to be rebuilt when the spacing changes.

use glow::HasContext;

use super::deferred::{Dead, build_pair, graveyard};

const VERTEX_SOURCE: &str = include_str!("grid.vert");
const FRAGMENT_SOURCE: &str = include_str!("grid.frag");

/// The corners of the quad, as a strip.
const CORNERS: [f32; 8] = [-1.0, -1.0, 1.0, -1.0, -1.0, 1.0, 1.0, 1.0];

/// Where the grid stands and how finely it is ruled, worked out from the model's own bounds.
pub struct Ground {
    pub view_projection: [f32; 16],
    /// The middle of the quad and how far it reaches: the camera, and everything the frustum can
    /// hold around it.
    pub center: [f32; 2],
    pub extent: f32,
    /// The near and far planes the frustum was built with.
    pub range: [f32; 2],
    pub step: f32,
}

#[derive(Default)]
pub struct Grid {
    program: Option<glow::Program>,
    layout: Option<(glow::VertexArray, glow::Buffer)>,
}

impl Grid {
    /// Draws it into `into`, tested against the depth that framebuffer already carries and writing
    /// none of its own.
    pub fn draw(
        &mut self,
        gl: &glow::Context,
        ground: &Ground,
        into: Option<glow::Framebuffer>,
        viewport: (i32, i32, i32, i32),
    ) -> Result<(), String> {
        let program = match self.program {
            Some(held) => held,
            None => *self
                .program
                .insert(build_pair(gl, VERTEX_SOURCE, FRAGMENT_SOURCE)?),
        };
        let (layout, _) = match self.layout {
            Some(held) => held,
            None => *self.layout.insert(upload(gl)?),
        };
        unsafe {
            gl.bind_framebuffer(glow::FRAMEBUFFER, into);
            gl.viewport(viewport.0, viewport.1, viewport.2, viewport.3);
            gl.enable(glow::DEPTH_TEST);
            gl.depth_func(glow::LESS);
            gl.depth_mask(false);
            gl.disable(glow::CULL_FACE);
            gl.color_mask(true, true, true, true);
            gl.enable(glow::BLEND);
            // Premultiplied, which is both what the shader layers its own lines with and what egui
            // blends the widget it is drawn over with.
            gl.blend_func(glow::ONE, glow::ONE_MINUS_SRC_ALPHA);

            gl.use_program(Some(program));
            let held = gl.get_uniform_location(program, "u_view_projection");
            gl.uniform_matrix_4_f32_slice(held.as_ref(), false, &ground.view_projection);
            let held = gl.get_uniform_location(program, "u_center");
            gl.uniform_2_f32_slice(held.as_ref(), &ground.center);
            let held = gl.get_uniform_location(program, "u_extent");
            gl.uniform_1_f32(held.as_ref(), ground.extent);
            let held = gl.get_uniform_location(program, "u_range");
            gl.uniform_2_f32_slice(held.as_ref(), &ground.range);
            let held = gl.get_uniform_location(program, "u_step");
            gl.uniform_1_f32(held.as_ref(), ground.step);

            gl.bind_vertex_array(Some(layout));
            gl.draw_arrays(glow::TRIANGLE_STRIP, 0, 4);
            gl.bind_vertex_array(None);
            gl.disable(glow::BLEND);
        }
        Ok(())
    }
}

fn upload(gl: &glow::Context) -> Result<(glow::VertexArray, glow::Buffer), String> {
    unsafe {
        let layout = gl.create_vertex_array()?;
        gl.bind_vertex_array(Some(layout));
        let held = gl.create_buffer()?;
        gl.bind_buffer(glow::ARRAY_BUFFER, Some(held));
        gl.buffer_data_u8_slice(
            glow::ARRAY_BUFFER,
            bytemuck::cast_slice(&CORNERS),
            glow::STATIC_DRAW,
        );
        gl.enable_vertex_attrib_array(0);
        gl.vertex_attrib_pointer_f32(0, 2, glow::FLOAT, false, 8, 0);
        gl.bind_vertex_array(None);
        gl.bind_buffer(glow::ARRAY_BUFFER, None);
        Ok((layout, held))
    }
}

impl Drop for Grid {
    fn drop(&mut self) {
        let mut dead = graveyard().lock().unwrap();
        dead.extend(self.program.take().map(Dead::Program));
        dead.extend(
            self.layout
                .take()
                .into_iter()
                .flat_map(|(layout, held)| [Dead::Layout(layout), Dead::Buffer(held)]),
        );
    }
}
