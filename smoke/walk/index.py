import xml.etree.ElementTree as ET, json, sys, collections, struct, zipfile, os

XML, OUT = sys.argv[1], sys.argv[2]
def named(e, n):
    for x in e.iter():
        if x.get('name') == n: return x
    return None

def spirv_name(data):
    """DXVK's debug name ('VS_<sha1>' / 'FS_<sha1>' / ...), read out of the module's own
    OpString rather than recomputed, so it survives whatever DXVK actually hashed."""
    if len(data) < 20: return None
    magic = struct.unpack_from('<I', data, 0)[0]
    if magic == 0x07230203: endian = '<'
    elif magic == 0x03022307: endian = '>'
    else: return None
    n = len(data)//4
    words = struct.unpack_from(f'{endian}{n}I', data, 0)
    i = 5
    while i < len(words):
        instr = words[i]; wc = instr >> 16; op = instr & 0xFFFF
        if wc == 0: break
        if op == 7:  # OpString
            raw = b''.join(struct.pack('<I', w) for w in words[i+2:i+wc])
            s = raw.split(b'\x00', 1)[0].decode('utf-8', 'replace')
            if s.startswith(('VS_', 'FS_', 'CS_', 'GS_', 'HS_', 'DS_')): return s
        i += wc
    return None

ZIP = XML[:-4] if XML.endswith('.xml') else XML + '.zip'
zf = None
if os.path.exists(ZIP):
    try: zf = zipfile.ZipFile(ZIP)
    except Exception: zf = None
name_cache = {}
def buf_name(buf):
    if buf is None or zf is None: return None
    if buf in name_cache: return name_cache[buf]
    try: data = zf.read(buf.zfill(6))
    except KeyError: data = None
    n = spirv_name(data) if data else None
    name_cache[buf] = n
    return n

images, views, sets, draws, passes, pipelines = {}, {}, {}, [], [], {}
bufmem, memblob, bufsize, shadermodules, samplers = {}, {}, {}, {}, {}
cur_pass, cur_pipe, cur_cull, bound = None, None, None, {}
events = []

def stage_entry(s):
    """(stage, module, buf, name) for one VkPipelineShaderStageCreateInfo struct: module id 0
    means DXVK supplied the SPIR-V inline via a pNext-chained VkShaderModuleCreateInfo, so pCode
    sits right there in this struct; a nonzero id means a real vkCreateShaderModule elsewhere."""
    stage = named(s, 'stage').get('string') or ''
    module = named(s, 'module')
    module = module.text.strip() if module is not None else None
    inline = named(s, 'pCode')
    if inline is not None:
        buf = inline.text.strip()
    else:
        buf = shadermodules.get(module)
    return [stage, module, buf, buf_name(buf)]

for ev, elem in ET.iterparse(XML, events=("end",)):
    if elem.tag != 'chunk': continue
    n = elem.get('name'); idx = int(elem.get('chunkIndex') or -1)
    if n == 'vkCreateImage':
        rid = named(elem,'Image'); ci = next((e for e in elem.iter() if e.get('typename')=='VkImageCreateInfo'), None)
        if rid is not None and ci is not None:
            ext = named(ci,'extent')
            images[rid.text.strip()] = {'w':int(named(ext,'width').text),'h':int(named(ext,'height').text),
                'fmt':(named(ci,'format').get('string') or ''),'layers':int(named(ci,'arrayLayers').text),
                'mips':int(named(ci,'mipLevels').text)}
    elif n == 'vkCreateImageView':
        rid = named(elem,'View'); img = named(elem,'image')
        if rid is not None and img is not None: views[rid.text.strip()] = img.text.strip()
    elif n == 'vkCreateBuffer':
        rid = named(elem,'Buffer'); sz = named(elem,'size')
        if rid is not None and sz is not None: bufsize[rid.text.strip()] = int(sz.text)
    elif n == 'vkBindBufferMemory':
        b = named(elem,'buffer'); m = named(elem,'memory'); o = named(elem,'memoryOffset')
        if b is not None: bufmem[b.text.strip()] = (m.text.strip(), int(o.text))
    elif n == 'vkCreateShaderModule':
        rid = named(elem, 'ShaderModule'); code = named(elem, 'pCode')
        if rid is not None and code is not None:
            shadermodules[rid.text.strip()] = code.text.strip()
    elif n == 'vkCreateSampler':
        rid = named(elem, 'Sampler')
        ci = next((e for e in elem.iter() if e.get('typename')=='VkSamplerCreateInfo'), None)
        if rid is not None and ci is not None:
            def enumstr(field):
                e = named(ci, field)
                return e.get('string') if e is not None else None
            def val(field):
                e = named(ci, field)
                return e.text.strip() if e is not None else None
            samplers[rid.text.strip()] = {
                'u': enumstr('addressModeU'), 'v': enumstr('addressModeV'), 'w': enumstr('addressModeW'),
                'bias': float(val('mipLodBias') or 0), 'aniso': int(val('anisotropyEnable') or 0),
                'maxaniso': float(val('maxAnisotropy') or 0),
                'minlod': float(val('minLod') or 0), 'maxlod': float(val('maxLod') or 0),
                'mipmap': enumstr('mipmapMode'), 'minfilter': enumstr('minFilter'), 'magfilter': enumstr('magFilter'),
                'border': enumstr('borderColor'),
            }
    elif n == 'Internal::Initial Contents':
        t = named(elem,'type').get('string')
        rid = named(elem,'id').text.strip()
        if t == 'eResDeviceMemory':
            buf = next((e for e in elem.iter() if e.tag=='buffer' and e.get('name')=='Contents'), None)
            if buf is not None: memblob[rid] = {'blob': buf.text.strip(), 'size': int(buf.get('byteLength'))}
        elif t == 'eResDescriptorSet':
            slots = []
            for s in elem.iter():
                if s.tag=='struct' and s.get('typename')=='DescriptorSetSlot':
                    ty = named(s,'type').get('string').replace('VK_DESCRIPTOR_TYPE_','')
                    res = named(s,'resource'); off = named(s,'offset'); rng = named(s,'range')
                    smp = named(s,'sampler')
                    slots.append({'t':ty,'r':res.text.strip() if res is not None else None,
                        'o':int(off.text) if off is not None else None,
                        'n':int(rng.text) if rng is not None else None,
                        's':smp.text.strip() if smp is not None else None})
            sets[rid] = slots
    elif n == 'vkUpdateDescriptorSets':
        for w in elem.iter():
            if w.tag=='struct' and w.get('typename')=='VkWriteDescriptorSet':
                dst = named(w,'dstSet'); b = named(w,'dstBinding')
                if dst is None: continue
                key = dst.text.strip(); at = int(b.text)
                slots = sets.setdefault(key, [])
                while len(slots) <= at: slots.append(None)
                ty = named(w,'descriptorType').get('string').replace('VK_DESCRIPTOR_TYPE_','')
                rec = {'t':ty}
                for s in w.iter():
                    if s.tag=='struct' and s.get('typename')=='VkDescriptorBufferInfo':
                        rec['r']=named(s,'buffer').text.strip(); rec['o']=int(named(s,'offset').text); rec['n']=int(named(s,'range').text)
                    if s.tag=='struct' and s.get('typename')=='VkDescriptorImageInfo':
                        iv = named(s,'imageView')
                        if iv is not None: rec['r']=iv.text.strip()
                        smp = named(s,'sampler')
                        if smp is not None: rec['s']=smp.text.strip()
                slots[at] = rec
    elif n == 'vkCreateGraphicsPipelines':
        rid = named(elem,'Pipeline')
        stages = [stage_entry(s) for s in elem.iter()
                  if s.tag=='struct' and s.get('typename')=='VkPipelineShaderStageCreateInfo']
        if rid is not None: pipelines[rid.text.strip()] = stages
    elif n == 'vkCreateComputePipelines':
        rid = named(elem,'Pipeline')
        s = next((e for e in elem.iter() if e.tag=='struct' and e.get('typename')=='VkPipelineShaderStageCreateInfo'), None)
        if rid is not None and s is not None: pipelines[rid.text.strip()] = [stage_entry(s)]
    elif n == 'vkCmdBeginRendering':
        col, dep = [], None
        for e in elem.iter():
            if e.tag=='struct' and e.get('typename')=='VkRenderingAttachmentInfo':
                iv = named(e,'imageView'); nm = e.get('name') or ''
                v = iv.text.strip() if iv is not None else None
                if 'epth' in nm: dep = v
                elif 'tencil' not in nm: col.append(v)
        ext = next(((int(named(e,'width').text), int(named(e,'height').text))
                    for e in elem.iter() if e.tag=='struct' and e.get('typename')=='VkRect2D'), None)
        cur_pass = len(passes)
        passes.append({'idx':idx,'color':col,'depth':dep,'extent':ext})
    elif n == 'vkCmdEndRendering': cur_pass = None
    elif n == 'vkCmdBindPipeline':
        p = named(elem,'pipeline'); cur_pipe = p.text.strip() if p is not None else None
    elif n == 'vkCmdSetCullMode':
        c = named(elem,'cullMode'); cur_cull = c.get('string') if c is not None else None
    elif n == 'vkCmdBindDescriptorSets':
        first = int(named(elem,'firstSet').text)
        got = next(([x.text.strip() for x in e if x.tag == 'ResourceId'] for e in elem.iter() if e.get('name')=='pDescriptorSets'), [])
        for at, s in enumerate(got): bound[first+at] = s
    elif n in ('vkCmdDrawIndexed','vkCmdDraw'):
        cnt = named(elem,'indexCount'); cnt = cnt if cnt is not None else named(elem,'vertexCount')
        inst = named(elem,'instanceCount')
        draws.append({'idx':idx,'pass':cur_pass,'pipe':cur_pipe,'cull':cur_cull,
            'count':int(cnt.text) if cnt is not None else 0,
            'instances':int(inst.text) if inst is not None else 0, 'sets':dict(bound)})
    elem.clear()

json.dump({'images':images,'views':views,'sets':sets,'draws':draws,'passes':passes,
           'pipelines':pipelines,'bufmem':bufmem,'memblob':memblob,'bufsize':bufsize,
           'samplers':samplers}, open(OUT,'w'))
named_shaders = sum(1 for s in pipelines.values() for e in s if e[3])
total_shaders = sum(len(s) for s in pipelines.values())
print(len(draws),'draws',len(sets),'sets',len(memblob),'memory blobs',len(pipelines),'pipelines',
      f'{named_shaders}/{total_shaders} shader stages named', ('(no zip)' if zf is None else ''))
