// Injected before the app's own scripts. Counts the GL work the renderer actually does, so a
// mis-aimed click fails the run instead of passing with nothing drawn.
(() => {
    const counters = {
        contexts: 0,
        samples: null,
        antialias: null,
        depthBits: null,
        depth: null,
        maxDrawBuffers: null,
        draws: 0,
        instanced: 0,
        blits: 0,
        links: 0,
        programs: 0,
        drawBuffers: 0,
        renderer: null,
    };
    globalThis.__smoke = counters;

    const wrap = (gl) => {
        const tally = (name, key) => {
            const original = gl[name];
            if (typeof original !== "function") return;
            gl[name] = function (...args) {
                counters[key]++;
                return original.apply(this, args);
            };
        };
        tally("drawArrays", "draws");
        tally("drawElements", "draws");
        tally("drawArraysInstanced", "instanced");
        tally("drawElementsInstanced", "instanced");
        tally("blitFramebuffer", "blits");
        tally("drawBuffers", "drawBuffers");

        const link = gl.linkProgram;
        gl.linkProgram = function (program) {
            counters.links++;
            const out = link.apply(this, arguments);
            if (this.getProgramParameter(program, this.LINK_STATUS)) counters.programs++;
            return out;
        };
    };

    const getContext = HTMLCanvasElement.prototype.getContext;
    HTMLCanvasElement.prototype.getContext = function (kind, attributes) {
        const gl = getContext.call(this, kind, attributes);
        if (gl && (kind === "webgl2" || kind === "webgl")) {
            counters.contexts++;
            try {
                counters.samples = gl.getParameter(gl.SAMPLES);
                counters.antialias = gl.getContextAttributes().antialias;
                // What the canvas itself was given, which is what the model viewer used to depth
                // test against: eframe asks for no attributes, so this is the WebGL default.
                counters.depthBits = gl.getParameter(gl.DEPTH_BITS);
                counters.depth = gl.getContextAttributes().depth;
                // A model's G-buffer pages to this many attachments until asked, so a gate whose
                // context answers 4 never exercises the page split a wider answer would ask for.
                counters.maxDrawBuffers = gl.getParameter(gl.MAX_DRAW_BUFFERS);
                const info = gl.getExtension("WEBGL_debug_renderer_info");
                counters.renderer = info
                    ? gl.getParameter(info.UNMASKED_RENDERER_WEBGL)
                    : gl.getParameter(gl.RENDERER);
            } catch (e) {
                /* a lost context has nothing to report */
            }
            wrap(gl);
        }
        return gl;
    };
})();
