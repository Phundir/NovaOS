#!/usr/bin/env python3
"""Generates the Venus encoder of the Nova kernel from the Vulkan registry.

    tools/gen-venus.py [--xml DIR] [--out src/gpu/venus/protocol.rs]

Venus is the protocol virtio-gpu speaks to the host's Vulkan driver
(spec 8.5): Vulkan commands serialized into a byte stream, by the rules of
VK_EXT_command_serialization. The rules are mechanical -- a scalar is 4 or
8 bytes, a pointer is a count followed by the values, a struct is its
members in order -- but they apply to hundreds of structures, and a field
one word off makes the host decode everything after it as garbage. So the
encoder is not written by hand: this tool reads the registry and writes
it, for the commands Nova uses and the structures they reach, nothing
else.

Inputs (fetched by `make venus`, checked against the sums below):
  vk.xml                            the Vulkan registry
  VK_EXT_command_serialization.xml  the command type numbers
  VK_MESA_venus_protocol.xml        the Venus transport commands

Output: one Rust file, committed, that the kernel compiles like any other.
The kernel build never needs the network or Python; `make venus` is run
when the list below changes, as `make police` is for the font.

What is generated, per type:
  - input structures (what the kernel sends): Rust structs borrowing their
    arrays and strings, an encoder for each;
  - output structures (what the host answers): owned Rust structs, a
    decoder, and the "partial" encoder the command itself carries (only
    sType, pNext, handles and counts of an output are sent);
  - per command: an encoder of the command, and a decoder of its reply.

The rules implemented here are those of the specification of
VK_EXT_command_serialization and of the Venus reference encoder, including
its special cases (the queue family indices of a non-concurrent resource
are not sent; a union is sent with a fixed tag). `tools/venus-check/`
compares the bytes this encoder produces against the reference encoder's.
"""

import argparse
import hashlib
import re
import sys
import xml.etree.ElementTree as ET
from pathlib import Path

# Pinned inputs: venus-protocol at this commit (vk.xml 1.4.334's structures,
# command numbers up to the renderer of virglrenderer 1.3.0 and beyond).
VENUS_PROTOCOL_COMMIT = 'ca19b6358d7cc491bc3e4de76f04c6700876a8fa'
XML_SUMS = {
    'vk.xml': '264d0d7350e37d70c82407fb430d085040fc01a9a961d43dec8c2d6ed1dfd183',
    'VK_EXT_command_serialization.xml': '2451e5dcc5306f604c52da48a8cc883a24de708dd86f38bbb035d29a753a0474',
    'VK_MESA_venus_protocol.xml': 'd92839bc728fa9ad9a7decdc6b91df6fa1a0fb26cffae4009865f18a789e0535',
}

# The commands the kernel sends. Only these, and what they reach, are
# generated: every line here is protocol surface the kernel exposes to the
# host, and every line not here is code that does not exist.
COMMANDS = [
    # transport
    'vkSetReplyCommandStreamMESA',
    'vkSeekReplyCommandStreamMESA',
    # the command ring (13.5.2)
    'vkCreateRingMESA',
    'vkDestroyRingMESA',
    'vkNotifyRingMESA',
    # instance and physical device
    'vkCreateInstance',
    'vkDestroyInstance',
    'vkEnumerateInstanceVersion',
    'vkEnumeratePhysicalDevices',
    'vkGetPhysicalDeviceProperties',
    'vkGetPhysicalDeviceFeatures',
    'vkGetPhysicalDeviceQueueFamilyProperties',
    'vkGetPhysicalDeviceMemoryProperties',
    'vkGetPhysicalDeviceFormatProperties',
    'vkEnumerateDeviceExtensionProperties',
    # device and queue
    'vkCreateDevice',
    'vkDestroyDevice',
    'vkGetDeviceQueue2',
    'vkQueueSubmit',
    # memory
    'vkAllocateMemory',
    'vkFreeMemory',
    'vkGetBufferMemoryRequirements',
    'vkGetImageMemoryRequirements',
    'vkBindBufferMemory',
    'vkBindImageMemory',
    # objects
    'vkCreateBuffer',
    'vkDestroyBuffer',
    'vkCreateImage',
    'vkDestroyImage',
    'vkCreateImageView',
    'vkDestroyImageView',
    'vkCreateSampler',
    'vkDestroySampler',
    'vkCreateShaderModule',
    'vkDestroyShaderModule',
    'vkCreateDescriptorSetLayout',
    'vkDestroyDescriptorSetLayout',
    'vkCreatePipelineLayout',
    'vkDestroyPipelineLayout',
    'vkCreateGraphicsPipelines',
    'vkDestroyPipeline',
    'vkCreateCommandPool',
    'vkDestroyCommandPool',
    'vkAllocateCommandBuffers',
    'vkFreeCommandBuffers',
    # recording
    'vkBeginCommandBuffer',
    'vkEndCommandBuffer',
    'vkResetCommandBuffer',
    'vkCmdBeginRendering',
    'vkCmdEndRendering',
    'vkCmdBindPipeline',
    'vkCmdBindVertexBuffers',
    'vkCmdBindIndexBuffer',
    'vkCmdPushDescriptorSet',
    'vkCmdPushConstants',
    'vkCmdSetViewport',
    'vkCmdSetScissor',
    'vkCmdDraw',
    'vkCmdDrawIndexed',
    'vkCmdCopyBuffer',
    'vkCmdCopyBufferToImage',
    'vkCmdCopyImageToBuffer',
    'vkCmdCopyImage',
    'vkCmdBlitImage',
    'vkCmdPipelineBarrier2',
    'vkCmdUpdateBuffer',
    'vkCmdFillBuffer',
    'vkCmdClearColorImage',
]

# Structures the kernel puts in a pNext chain. A chain may only carry
# these; each goes on the bases its `structextends` names.
CHAIN = [
    'VkDeviceQueueTimelineInfoMESA',
    'VkPhysicalDeviceVulkan11Features',
    'VkPhysicalDeviceVulkan12Features',
    'VkPhysicalDeviceVulkan13Features',
    'VkPipelineRenderingCreateInfo',
    'VkExportMemoryAllocateInfo',
    'VkImportMemoryResourceInfoMESA',
    'VkMemoryDedicatedAllocateInfo',
    'VkExternalMemoryImageCreateInfo',
    'VkExternalMemoryBufferCreateInfo',
]

# Enumerations whose values the kernel names (module `vk` of the output).
ENUMS = [
    'VkResult', 'VkPhysicalDeviceType', 'VkQueueFlagBits', 'VkMemoryPropertyFlagBits',
    'VkMemoryHeapFlagBits', 'VkFormat', 'VkFormatFeatureFlagBits', 'VkImageUsageFlagBits',
    'VkBufferUsageFlagBits', 'VkImageLayout', 'VkImageType', 'VkImageTiling',
    'VkSampleCountFlagBits', 'VkSharingMode', 'VkImageViewType', 'VkImageAspectFlagBits',
    'VkComponentSwizzle', 'VkFilter', 'VkSamplerMipmapMode', 'VkSamplerAddressMode',
    'VkBorderColor', 'VkDescriptorType', 'VkShaderStageFlagBits',
    'VkDescriptorSetLayoutCreateFlagBits', 'VkPipelineBindPoint', 'VkPrimitiveTopology',
    'VkPolygonMode', 'VkCullModeFlagBits', 'VkFrontFace', 'VkCompareOp', 'VkBlendFactor',
    'VkBlendOp', 'VkColorComponentFlagBits', 'VkDynamicState', 'VkVertexInputRate',
    'VkAttachmentLoadOp', 'VkAttachmentStoreOp', 'VkIndexType', 'VkCommandPoolCreateFlagBits',
    'VkCommandBufferLevel', 'VkCommandBufferUsageFlagBits', 'VkPipelineStageFlagBits2',
    'VkAccessFlagBits2', 'VkExternalMemoryHandleTypeFlagBits', 'VkResolveModeFlagBits',
    'VkLogicOp', 'VkStencilOp', 'VkCommandBufferResetFlagBits',
]

# Arrays the reference encoder sends only under a condition (its
# IGNORABLE_LIST): otherwise their count is sent, the array as empty.
CONDITIONAL = {
    ('VkImageCreateInfo', 'pQueueFamilyIndices'): 'self.sharing_mode == 1',
    ('VkBufferCreateInfo', 'pQueueFamilyIndices'): 'self.sharing_mode == 1',
}

# Unions, always sent with the reference encoder's default tag. Both are
# 16 bytes of clear value; the kernel holds them as four words.
UNIONS = {'VkClearValue', 'VkClearColorValue'}

SCALARS = {
    # C type: (Rust type, encoder, decoder)
    'uint32_t': ('u32', 'u32', 'u32'),
    'int32_t': ('i32', 'i32', 'i32'),
    'uint64_t': ('u64', 'u64', 'u64'),
    'int64_t': ('i64', 'i64', 'i64'),
    'size_t': ('u64', 'u64', 'u64'),
    # Floating point crosses the kernel as its bit pattern: the kernel is
    # soft-float and never computes with it (CLAUDE.md).
    'float': ('u32', 'u32', 'u32'),
    'double': ('u64', 'u64', 'u64'),
    'VkBool32': ('u32', 'u32', 'u32'),
    'VkSampleMask': ('u32', 'u32', 'u32'),
    'VkDeviceSize': ('u64', 'u64', 'u64'),
    'VkDeviceAddress': ('u64', 'u64', 'u64'),
    'VkFlags': ('u32', 'u32', 'u32'),
    'VkFlags64': ('u64', 'u64', 'u64'),
}

RUST_KEYWORDS = {'type', 'ref', 'mod', 'fn', 'impl', 'loop', 'match', 'move', 'use', 'where', 'box', 'in'}


def fail(message):
    sys.exit('gen-venus: ' + message)


def snake(name):
    """Vulkan member name to a Rust field name: pNext -> next,
    pQueueFamilyIndices -> queue_family_indices."""
    stripped = re.sub(r'^pp?(?=[A-Z])', '', name)
    words = re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', stripped)
    words = re.sub(r'([A-Z]+)([A-Z][a-z])', r'\1_\2', words).lower()
    if words in RUST_KEYWORDS:
        words += '_'
    return words


def api_ok(element):
    api = element.get('api')
    return api is None or 'vulkan' in api.split(',')


class Var:
    """A struct member or a command parameter."""

    def __init__(self, element):
        name = element.find('name')
        type_ = element.find('type')
        self.name = name.text
        self.type = type_.text
        # Read from the XML layout itself: `const` before <type>, the stars
        # between </type> and <name>, the array bound after </name>.
        self.const = (element.text or '').strip().startswith('const')
        self.pointers = (type_.tail or '').count('*')
        self.fixed = None
        tail = name.tail or ''
        if tail.strip().startswith('['):
            enum = element.find('enum')
            if enum is not None:
                self.fixed = enum.text
            else:
                self.fixed = re.search(r'\[(\d+)\]', tail).group(1)
        self.len = element.get('len')
        self.altlen = element.get('altlen')
        self.optional = element.get('optional', '')
        self.noauto = element.get('noautovalidity') == 'true'
        self.values = element.get('values')
        self.field = snake(self.name)

    def len_parts(self):
        return [part.strip() for part in self.len.split(',')] if self.len else []


class Registry:
    def __init__(self, directory):
        self.types = {}  # name -> element (structs/unions/handles/enums/bitmasks/basetypes)
        self.aliases = {}
        self.categories = {}
        self.commands = {}
        self.command_aliases = {}
        self.constants = {}
        self.enum_values = {}  # enum constant name -> int
        self.command_types = {}
        self.extnumber = {}
        self.enum_type = {}  # enum constant name -> the enum type it belongs to
        roots = [ET.parse(directory / name).getroot() for name in XML_SUMS]
        for root in roots:
            self._read_types(root)
            self._read_enums(root)
            self._read_commands(root)
        for root in roots:
            self._read_extension_enums(root)

    def _read_types(self, root):
        for types in root.findall('types'):
            for t in types.findall('type'):
                if not api_ok(t):
                    continue
                name = t.get('name') or (t.find('name').text if t.find('name') is not None else None)
                if name is None:
                    continue
                if t.get('alias'):
                    self.aliases[name] = t.get('alias')
                    continue
                category = t.get('category')
                if name in self.types and category is None:
                    continue
                self.types[name] = t
                self.categories[name] = category

    def _read_enums(self, root):
        for enums in root.findall('enums'):
            for e in enums.findall('enum'):
                if not api_ok(e):
                    continue
                name = e.get('name')
                if e.get('alias'):
                    continue
                value = e.get('value')
                if value is not None:
                    value = value.strip('()').replace('U', '').replace('L', '')
                    if value.startswith('"') or '~' in value or 'f' in value.lower() and not value.startswith('0x'):
                        continue
                    try:
                        number = int(value, 0)
                    except ValueError:
                        continue
                    if enums.get('name') == 'API Constants':
                        self.constants[name] = number
                    elif enums.get('name') == 'VkCommandTypeEXT':
                        self.command_types[name] = number
                    else:
                        self.enum_values[name] = number
                        self.enum_type[name] = enums.get('name')
                elif e.get('bitpos') is not None:
                    self.enum_values[name] = 1 << int(e.get('bitpos'))
                    self.enum_type[name] = enums.get('name')

    def _read_commands(self, root):
        for commands in root.findall('commands'):
            for c in commands.findall('command'):
                if not api_ok(c):
                    continue
                if c.get('alias'):
                    self.command_aliases[c.get('name')] = c.get('alias')
                    continue
                name = c.find('proto/name').text
                self.commands[name] = c

    def _extension_value(self, e, number):
        if e.get('value') is not None:
            return int(e.get('value'), 0)
        if e.get('bitpos') is not None:
            return 1 << int(e.get('bitpos'))
        if e.get('offset') is not None:
            ext = int(e.get('extnumber') or number)
            value = 1000000000 + (ext - 1) * 1000 + int(e.get('offset'))
            return -value if e.get('dir') == '-' else value
        return None

    def _read_extension_enums(self, root):
        def walk(parent, number):
            for require in parent.findall('require'):
                for e in require.findall('enum'):
                    if not api_ok(e) or e.get('extends') is None or e.get('alias'):
                        continue
                    value = self._extension_value(e, number)
                    if value is not None:
                        self.enum_values.setdefault(e.get('name'), value)
                        self.enum_type.setdefault(e.get('name'), e.get('extends'))
        for feature in root.findall('feature'):
            walk(feature, None)
        for extensions in root.findall('extensions'):
            for ext in extensions.findall('extension'):
                walk(ext, ext.get('number'))

    def resolve(self, name):
        while name in self.aliases:
            name = self.aliases[name]
        return name

    def members(self, name):
        element = self.types[self.resolve(name)]
        return [Var(m) for m in element.findall('member') if api_ok(m)]

    def category(self, name):
        name = self.resolve(name)
        return self.categories.get(name)

    def constant(self, token):
        if token.isdigit():
            return int(token)
        return self.constants[token]

    def stype(self, struct):
        for m in self.members(struct):
            if m.name == 'sType':
                return self.enum_values[m.values]
        return None

    def extends(self, struct):
        element = self.types[self.resolve(struct)]
        value = element.get('structextends')
        return value.split(',') if value else []


class Gen:
    def __init__(self, reg):
        self.reg = reg
        self.inputs = []   # struct names encoded, in dependency order
        self.outputs = []  # struct names decoded
        self.out = []
        self.chain = [reg.resolve(c) for c in CHAIN]

    # ---- classification ----------------------------------------------

    def kind(self, var):
        """What a member or parameter is, for the encoder."""
        t = self.reg.resolve(var.type)
        cat = self.reg.category(t)
        if var.name == 'pNext':
            return 'next'
        if var.name == 'sType':
            return 'stype'
        if var.type == 'char' and var.pointers == 1:
            return 'string'
        if var.type == 'char' and var.pointers == 2:
            return 'strings'
        if var.type in ('char', 'uint8_t') and var.fixed:
            return 'chars'
        if var.type == 'void' and var.pointers == 1:
            return 'blob'
        if var.fixed:
            return 'fixed'
        if var.pointers == 1:
            return 'array' if var.len else 'pointer'
        if t in UNIONS:
            return 'union'
        if cat == 'handle':
            return 'handle'
        if cat == 'struct':
            return 'struct'
        if cat in ('enum',):
            return 'enum'
        if cat == 'bitmask':
            return 'scalar'
        if t in SCALARS or cat == 'basetype':
            return 'scalar'
        fail('type inconnu : %s %s' % (var.type, var.name))

    def scalar(self, type_name):
        t = self.reg.resolve(type_name)
        cat = self.reg.category(t)
        if cat == 'enum':
            return ('i32', 'i32', 'i32')
        if cat == 'handle':
            return ('u64', 'u64', 'u64')
        if cat == 'bitmask':
            element = self.reg.types[t]
            base = element.find('type').text if element.find('type') is not None else 'VkFlags'
            return SCALARS[base]
        if t in SCALARS:
            return SCALARS[t]
        if cat == 'basetype':
            element = self.reg.types[t]
            inner = element.find('type')
            if inner is not None and inner.text in SCALARS:
                return SCALARS[inner.text]
        fail('scalaire inconnu : %s' % type_name)

    def element_rust(self, type_name, lifetime):
        t = self.reg.resolve(type_name)
        cat = self.reg.category(t)
        if t in UNIONS:
            return '[u32; 4]'
        if cat == 'struct':
            return t + ('<%s>' % lifetime if lifetime and self.borrows(t) else '')
        return self.scalar(t)[0]

    def borrows(self, struct):
        """Does the input form of this struct hold references? A structure
        of a chain holds none for its own pNext: the chain is the slice
        of its base."""
        in_chain = self.reg.resolve(struct) in self.chain
        for m in self.reg.members(struct):
            k = self.kind(m)
            if k == 'next' and in_chain:
                continue
            if k in ('next', 'string', 'strings', 'blob', 'array', 'pointer'):
                return True
            if k in ('struct', 'fixed') and self.reg.category(m.type) == 'struct' and self.borrows(m.type):
                return True
        return False

    # ---- closure -------------------------------------------------------

    def collect(self):
        seen_in, seen_out = set(), set()

        def add_in(name):
            name = self.reg.resolve(name)
            if name in seen_in or name in UNIONS:
                return
            seen_in.add(name)
            for m in self.reg.members(name):
                if m.name in ('sType', 'pNext'):
                    continue
                if self.reg.category(m.type) == 'struct':
                    add_in(m.type)
            self.inputs.append(name)

        def add_out(name):
            name = self.reg.resolve(name)
            if name in seen_out:
                return
            seen_out.add(name)
            for m in self.reg.members(name):
                if m.name == 'pNext':
                    fail('%s : une sortie avec chaine pNext n\'est pas prise en charge' % name)
                if m.pointers and m.name != 'pNext':
                    fail('%s : une sortie avec pointeur n\'est pas prise en charge' % name)
                if self.reg.category(m.type) == 'struct':
                    add_out(m.type)
            self.outputs.append(name)

        for c in self.chain:
            add_in(c)
        for command in COMMANDS:
            for p in self.params(command):
                cat = self.reg.category(p.type)
                if cat != 'struct' or p.name == 'pAllocator':
                    continue
                if p.pointers and not p.const:
                    add_out(p.type)
                else:
                    add_in(p.type)

    def params(self, command):
        element = self.reg.commands[self.reg.command_aliases.get(command, command)]
        return [Var(p) for p in element.findall('param') if api_ok(p)]

    # ---- counts ----------------------------------------------------------

    def derived_counts(self, members):
        """Count members computed from the slices they size: those whose
        every array is plain -- not optional, not conditional, not sized
        by an expression."""
        users = {}
        for m in members:
            if m.len and self.kind(m) in ('array', 'strings', 'blob'):
                count = m.len_parts()[0]
                users.setdefault(count, []).append(m)
        derived = {}
        names = {m.name: m for m in members}
        for count, arrays in users.items():
            if count not in names:
                continue
            plain = all(not a.optional.startswith('true') and not a.noauto and not a.altlen for a in arrays)
            if plain and self.kind(names[count]) == 'scalar':
                derived[count] = arrays[0]
        return derived

    # ---- emission helpers ------------------------------------------------

    def w(self, line=''):
        self.out.append(line)

    def count_expr(self, var, owner, derived, prefix):
        """Rust expression of the element count of an array member."""
        if var.altlen:
            expr = var.altlen
            for name in re.findall(r'[A-Za-z_]\w*', var.altlen):
                expr = re.sub(r'\b%s\b' % name, '(%s%s as u64)' % (prefix, snake(name)), expr)
            return '(%s) as u64' % expr
        count = var.len_parts()[0]
        if '->' in count or '::' in count:
            return None
        if count in derived:
            return '%s%s.len() as u64' % (prefix, snake(derived[count].name))
        return '%s%s as u64' % (prefix, snake(count))

    # ---- input structures ------------------------------------------------

    def input_field(self, m, derived):
        k = self.kind(m)
        if k in ('stype', 'next'):
            return None
        if m.name in derived:
            return None
        if k == 'scalar' or k == 'enum' or k == 'handle':
            return self.scalar(m.type)[0]
        if k == 'union':
            return '[u32; 4]'
        if k == 'struct':
            return self.element_rust(m.type, "'a")
        if k == 'fixed':
            return '[%s; %d]' % (self.element_rust(m.type, "'a"), self.reg.constant(m.fixed))
        if k == 'chars':
            return '[u8; %d]' % self.reg.constant(m.fixed)
        if k == 'string':
            return "Option<&'a str>"
        if k == 'strings':
            return "&'a [&'a str]"
        if k == 'blob':
            return "Option<&'a [u8]>" if m.optional.startswith('true') or m.noauto else "&'a [u8]"
        if k == 'array':
            inner = self.element_rust(m.type, "'a")
            count = m.len_parts()[0]
            if count in derived and derived[count] is not None and m.name == derived[count].name:
                return "&'a [%s]" % inner
            if count in derived:
                return "&'a [%s]" % inner
            return "Option<&'a [%s]>" % inner
        if k == 'pointer':
            return "Option<&'a %s>" % self.element_rust(m.type, "'a")
        fail('champ %s non gere' % m.name)

    def emit_input_struct(self, name):
        members = self.reg.members(name)
        derived = self.derived_counts(members)
        lifetime = self.borrows(name)
        has_next = any(m.name == 'pNext' for m in members) and name not in self.chain
        self.w('#[derive(Clone, Copy, Default)]')
        self.w('pub struct %s%s {' % (name, "<'a>" if lifetime else ''))
        if has_next:
            self.w("    /// pNext chain, head first.")
            self.w("    pub next: &'a [VkNext<'a>],")
        for m in members:
            ty = self.input_field(m, derived)
            if ty is None:
                continue
            if self.kind(m) == 'array' and m.len_parts()[0] in derived and ty.startswith('&'):
                self.w('    /// Also sets `%s`.' % m.len_parts()[0])
            self.w('    pub %s: %s,' % (m.field, ty))
        self.w('}')
        self.w()
        self.w('impl%s %s%s {' % ("<'a>" if lifetime else '', name, "<'a>" if lifetime else ''))
        self.w('    pub fn encode(&self, e: &mut Enc) {')
        stype = self.reg.stype(name)
        if stype is not None:
            self.w('        e.i32(%d);' % stype)
            if has_next:
                self.w('        encode_next_%s(e, self.next);' % name)
            elif name in self.chain:
                self.w('        e.u64(0);')
            else:
                self.w('        e.u64(0);')
        self.w('        self.encode_self(e);')
        self.w('    }')
        self.w()
        self.w('    pub fn encode_self(&self, e: &mut Enc) {')
        for m in members:
            self.emit_encode_member(name, m, derived, 'self.')
        self.w('    }')
        self.w('}')
        self.w()

    def emit_encode_member(self, owner, m, derived, prefix, indent='        '):
        k = self.kind(m)
        v = prefix + m.field
        w = lambda line: self.w(indent + line)
        if k in ('stype', 'next'):
            return
        if m.name in derived:
            w('e.%s(%s%s.len() as _);' % (self.scalar(m.type)[1], prefix, derived[m.name].field))
            return
        if k in ('scalar', 'enum', 'handle'):
            w('e.%s(%s);' % (self.scalar(m.type)[1], v))
        elif k == 'union':
            w('e.%s(&%s);' % ('clear_value' if self.reg.resolve(m.type) == 'VkClearValue' else 'clear_color', v))
        elif k == 'struct':
            w('%s.encode(e);' % v)
        elif k == 'fixed':
            n = self.reg.constant(m.fixed)
            w('e.array_size(%d);' % n)
            if self.reg.category(m.type) == 'struct':
                w('for item in %s.iter() { item.encode(e); }' % v)
            else:
                w('for item in %s.iter() { e.%s(*item); }' % (v, self.scalar(m.type)[1]))
        elif k == 'chars':
            w('e.chars(&%s);' % v)
        elif k == 'string':
            w('e.string(%s);' % v)
        elif k == 'strings':
            w('e.array_size(%s.len() as u64);' % v)
            w('for item in %s.iter() { e.string(Some(item)); }' % v)
        elif k == 'blob':
            count = self.count_expr(m, owner, derived, prefix)
            opt = m.optional.startswith('true') or m.noauto
            if opt:
                w('match %s {' % v)
                w('    Some(data) => { e.array_size(%s); e.blob(data, %s); }' % (count, count))
                w('    None => e.array_size(0),')
                w('}')
            else:
                w('e.array_size(%s); e.blob(%s, %s);' % (count, v, count))
        elif k == 'array':
            count = m.len_parts()[0]
            element = self.reg.resolve(m.type)
            is_struct = self.reg.category(element) == 'struct'
            item = 'item.encode(e);' if is_struct else 'e.%s(*item);' % self.scalar(element)[1]
            cond = CONDITIONAL.get((owner, m.name))
            if count in derived:
                w('e.array_size(%s.len() as u64);' % v)
                w('for item in %s.iter() { %s }' % (v, item))
            else:
                expr = self.count_expr(m, owner, derived, prefix)
                guard = 'if %s' % cond.replace('self.', prefix) if cond else ''
                w('match %s {' % v)
                w('    Some(items) %s=> { e.array_items(%s, items.len()); for item in items.iter().take(%s as usize) { %s } }'
                  % (guard + ' ' if guard else '', expr, expr, item))
                w('    _ => e.array_size(0),')
                w('}')
        elif k == 'pointer':
            element = self.reg.resolve(m.type)
            is_struct = self.reg.category(element) == 'struct'
            w('match %s {' % v)
            if is_struct:
                w('    Some(value) => { e.u64(1); value.encode(e); }')
            else:
                w('    Some(value) => { e.u64(1); e.%s(*value); }' % self.scalar(element)[1])
            w('    None => e.u64(0),')
            w('}')
        else:
            fail('encodage de %s.%s' % (owner, m.name))

    def emit_chain(self):
        self.w('/// A structure of a pNext chain.')
        self.w('#[derive(Clone, Copy)]')
        self.w("pub enum VkNext<'a> {")
        for c in self.chain:
            self.w('    %s(%s%s),' % (c[2:], c, "<'a>" if self.borrows(c) else ''))
        self.w('}')
        self.w()
        self.w("impl VkNext<'_> {")
        self.w('    pub fn s_type(&self) -> i32 {')
        self.w('        match self {')
        for c in self.chain:
            self.w('            VkNext::%s(_) => %d,' % (c[2:], self.reg.stype(c)))
        self.w('        }')
        self.w('    }')
        self.w()
        self.w('    fn encode_self(&self, e: &mut Enc) {')
        self.w('        match self {')
        for c in self.chain:
            self.w('            VkNext::%s(value) => value.encode_self(e),' % c[2:])
        self.w('        }')
        self.w('    }')
        self.w('}')
        self.w()
        bases = set()
        for name in self.inputs:
            if any(m.name == 'pNext' for m in self.reg.members(name)) and name not in self.chain:
                bases.add(name)
        for base in sorted(bases):
            allowed = [c for c in self.chain if base in self.reg.extends(c)]
            self.w('/// The chain of a `%s`: the structures it may carry, head first;' % base)
            self.w('/// any other is skipped, as the reference encoder skips an unknown one.')
            self.w("fn encode_next_%s(e: &mut Enc, next: &[VkNext]) {" % base)
            if allowed:
                self.w('    for (index, item) in next.iter().enumerate() {')
                self.w('        let known = matches!(item, %s);' % ' | '.join('VkNext::%s(_)' % c[2:] for c in allowed))
                self.w('        if known {')
                self.w('            e.u64(1);')
                self.w('            e.i32(item.s_type());')
                self.w('            encode_next_%s(e, &next[index + 1..]);' % base)
                self.w('            item.encode_self(e);')
                self.w('            return;')
                self.w('        }')
                self.w('    }')
            else:
                self.w('    let _ = next;')
            self.w('    e.u64(0);')
            self.w('}')
            self.w()

    # ---- output structures -------------------------------------------------

    def output_field(self, m):
        k = self.kind(m)
        if k in ('scalar', 'enum', 'handle'):
            return self.scalar(m.type)[0], 'Default::default()'
        if k == 'struct':
            return self.reg.resolve(m.type), 'Default::default()'
        if k == 'chars':
            n = self.reg.constant(m.fixed)
            return '[u8; %d]' % n, '[0; %d]' % n
        if k == 'fixed':
            n = self.reg.constant(m.fixed)
            if self.reg.category(m.type) == 'struct':
                inner = self.reg.resolve(m.type)
                return '[%s; %d]' % (inner, n), '[%s::default_const(); %d]' % (inner, n)
            return '[%s; %d]' % (self.scalar(m.type)[0], n), '[0; %d]' % n
        fail('sortie %s non geree' % m.name)

    def emit_output_struct(self, name):
        members = self.reg.members(name)
        is_input = name in self.inputs
        if not is_input:
            self.w('#[derive(Clone, Copy)]')
            self.w('pub struct %s {' % name)
            for m in members:
                ty, _ = self.output_field(m)
                self.w('    pub %s: %s,' % (m.field, ty))
            self.w('}')
            self.w()
        self.w('impl %s {' % name)
        self.w('    pub const fn default_const() -> Self {')
        self.w('        %s {' % name)
        for m in members:
            ty, _ = self.output_field(m)
            k = self.kind(m)
            if k == 'struct':
                value = '%s::default_const()' % self.reg.resolve(m.type)
            elif k in ('chars',):
                value = '[0; %d]' % self.reg.constant(m.fixed)
            elif k == 'fixed':
                n = self.reg.constant(m.fixed)
                if self.reg.category(m.type) == 'struct':
                    value = '[%s::default_const(); %d]' % (self.reg.resolve(m.type), n)
                else:
                    value = '[0; %d]' % n
            else:
                value = '0'
            self.w('            %s: %s,' % (m.field, value))
        self.w('        }')
        self.w('    }')
        self.w()
        self.w('    pub fn decode(d: &mut Dec) -> Self {')
        self.w('        let mut value = Self::default_const();')
        for m in members:
            k = self.kind(m)
            f = 'value.' + m.field
            if k in ('scalar', 'enum', 'handle'):
                self.w('        %s = d.%s();' % (f, self.scalar(m.type)[2]))
            elif k == 'struct':
                self.w('        %s = %s::decode(d);' % (f, self.reg.resolve(m.type)))
            elif k == 'chars':
                self.w('        d.chars(&mut %s);' % f)
            elif k == 'fixed':
                n = self.reg.constant(m.fixed)
                self.w('        let count = d.array_size(%d);' % n)
                if self.reg.category(m.type) == 'struct':
                    self.w('        for item in %s.iter_mut().take(count) { *item = %s::decode(d); }' % (f, self.reg.resolve(m.type)))
                else:
                    self.w('        for item in %s.iter_mut().take(count) { *item = d.%s(); }' % (f, self.scalar(m.type)[2]))
        self.w('        value')
        self.w('    }')
        self.w()
        self.w('    /// What the command carries of an output: its handles, and the')
        self.w('    /// arrays of structures that may hold some.')
        self.w('    pub fn encode_partial(&self, e: &mut Enc) {')
        for m in members:
            k = self.kind(m)
            if k == 'handle':
                self.w('        e.u64(self.%s);' % m.field)
            elif k == 'struct':
                self.w('        self.%s.encode_partial(e);' % m.field)
            elif k == 'fixed' and self.reg.category(m.type) == 'struct':
                n = self.reg.constant(m.fixed)
                self.w('        e.array_size(%d);' % n)
                self.w('        for item in self.%s.iter() { item.encode_partial(e); }' % m.field)
        self.w('        let _ = e;')
        self.w('    }')
        self.w('}')
        self.w()
        if not is_input:
            self.w('impl Default for %s {' % name)
            self.w('    fn default() -> Self {')
            self.w('        Self::default_const()')
            self.w('    }')
            self.w('}')
            self.w()

    # ---- commands ------------------------------------------------------------

    def command_type(self, command):
        key = 'VK_COMMAND_TYPE_%s_EXT' % command
        if key not in self.reg.command_types:
            fail('numero de commande inconnu : %s' % command)
        return self.reg.command_types[key]

    def emit_command(self, command):
        element = self.reg.commands[self.reg.command_aliases.get(command, command)]
        ret = element.find('proto/type').text
        params = self.params(command)
        derived = self.derived_counts(params)
        number = self.command_type(command)
        fname = snake(command[2:])
        args = ['e: &mut Enc', 'cmd_flags: u32']
        body = []
        outs = []  # (kind, var) for the reply
        names = {p.name: p for p in params}
        # In/out counts: an output array sized by a count pointer.
        inout_counts = set()
        for p in params:
            if p.len and p.pointers == 1 and not p.const:
                c = p.len_parts()[0]
                if c in names and names[c].pointers == 1:
                    inout_counts.add(c)
        for p in params:
            k = self.kind(p)
            t = self.reg.resolve(p.type)
            cat = self.reg.category(t)
            field = p.field
            if p.name == 'pAllocator':
                body.append('e.u64(0);')
                continue
            is_out = p.pointers >= 1 and not p.const and not (p.type == 'void' and p.pointers == 1 and p.const)
            if p.name in inout_counts:
                args.append('%s: Option<u32>' % field)
                body.append('match %s { Some(count) => { e.u64(1); e.u32(count); } None => e.u64(0) }' % field)
                outs.append(('count', p))
                continue
            if is_out and k == 'pointer':
                if cat == 'handle':
                    args.append('%s: u64' % field)
                    body.append('e.u64(1); e.u64(%s);' % field)
                    outs.append(('handle', p))
                elif cat == 'struct':
                    body.append('e.u64(1); %s::default_const().encode_partial(e);' % t)
                    outs.append(('struct', p))
                else:
                    # An output scalar is not sent: only its pointer.
                    body.append('e.u64(1);')
                    outs.append(('scalar', p))
                continue
            if is_out and k == 'array':
                count = p.len_parts()[0]
                if cat == 'handle':
                    args.append('%s: Option<&[u64]>' % field)
                    body.append('match %s { Some(ids) => { e.array_size(ids.len() as u64); for id in ids { e.u64(*id); } } None => e.array_size(0) }' % field)
                    outs.append(('handles', p))
                elif cat == 'struct':
                    args.append('%s: bool' % field)
                    cexpr = '%s.unwrap_or(0)' % snake(count) if count in names else '0'
                    body.append('if %s { e.array_size(%s as u64); for _ in 0..%s { %s::default_const().encode_partial(e); } } else { e.array_size(0); }'
                                % (field, cexpr, cexpr, t))
                    outs.append(('structs', p))
                else:
                    fail('%s : tableau de sortie non gere' % command)
                continue
            if is_out and k == 'blob':
                fail('%s : blob de sortie non gere' % command)
            # inputs
            if p.name in derived:
                body.append('e.%s(%s.len() as _);' % (self.scalar(p.type)[1], derived[p.name].field))
                continue
            if k in ('scalar', 'enum', 'handle'):
                args.append('%s: %s' % (field, self.scalar(p.type)[0]))
                body.append('e.%s(%s);' % (self.scalar(p.type)[1], field))
            elif k == 'pointer' and t in UNIONS:
                args.append('%s: &[u32; 4]' % field)
                body.append('e.u64(1); e.%s(%s);' % ('clear_value' if t == 'VkClearValue' else 'clear_color', field))
            elif k == 'pointer' and cat == 'struct':
                life = "<'_>" if self.borrows(t) else ''
                if p.optional.startswith('true'):
                    args.append('%s: Option<&%s%s>' % (field, t, life))
                    body.append('match %s { Some(value) => { e.u64(1); value.encode(e); } None => e.u64(0) }' % field)
                else:
                    args.append('%s: &%s%s' % (field, t, life))
                    body.append('e.u64(1); %s.encode(e);' % field)
            elif k == 'array':
                inner = self.element_rust(t, "'_")
                item = 'item.encode(e);' if cat == 'struct' else 'e.%s(*item);' % self.scalar(t)[1]
                if p.len_parts()[0] in derived:
                    args.append('%s: &[%s]' % (field, inner))
                    body.append('e.array_size(%s.len() as u64); for item in %s.iter() { %s }' % (field, field, item))
                else:
                    expr = self.count_expr(p, command, derived, '')
                    args.append('%s: Option<&[%s]>' % (field, inner))
                    body.append('match %s { Some(items) => { e.array_items(%s, items.len()); for item in items.iter().take(%s as usize) { %s } } None => e.array_size(0) }'
                                % (field, expr, expr, item))
            elif k == 'blob':
                expr = self.count_expr(p, command, derived, '')
                args.append('%s: &[u8]' % field)
                body.append('e.array_size(%s); e.blob(%s, %s);' % (expr, field, expr))
            elif k == 'string':
                args.append('%s: Option<&str>' % field)
                body.append('e.string(%s);' % field)
            else:
                fail('%s : parametre %s non gere' % (command, p.name))
        self.w('/// `%s`.' % command)
        self.w('pub fn %s(%s) {' % (fname, ', '.join(args)))
        self.w('    e.u32(%d);' % number)
        self.w('    e.u32(cmd_flags);')
        for line in body:
            self.w('    ' + line)
        self.w('}')
        self.w()
        # --- reply ---
        if ret == 'void' and not outs:
            return
        rname = 'Reply' + command[2:]
        self.w('/// Reply of `%s`.' % command)
        self.w('#[derive(Default)]')
        self.w('pub struct %s {' % rname)
        if ret != 'void':
            self.w('    pub ret: %s,' % self.scalar(ret)[0])
        for kind, p in outs:
            t = self.reg.resolve(p.type)
            if kind == 'count':
                self.w('    pub %s: Option<u32>,' % p.field)
            elif kind == 'handle':
                self.w('    pub %s: Option<u64>,' % p.field)
            elif kind == 'struct':
                self.w('    pub %s: Option<%s>,' % (p.field, t))
            elif kind == 'scalar':
                self.w('    pub %s: Option<%s>,' % (p.field, self.scalar(t)[0]))
            elif kind == 'handles':
                self.w('    pub %s: alloc::vec::Vec<u64>,' % p.field)
            elif kind == 'structs':
                self.w('    pub %s: alloc::vec::Vec<%s>,' % (p.field, t))
        self.w('}')
        self.w()
        self.w('/// Decodes the reply of `%s`; `None` if it is another command\'s or' % command)
        self.w('/// runs past the stream.')
        self.w('pub fn reply_%s(d: &mut Dec) -> Option<%s> {' % (fname, rname))
        self.w('    if d.u32() != %d {' % number)
        self.w('        return None;')
        self.w('    }')
        self.w('    let mut reply = %s::default();' % rname)
        if ret != 'void':
            self.w('    reply.ret = d.%s();' % self.scalar(ret)[2])
        for kind, p in outs:
            t = self.reg.resolve(p.type)
            f = 'reply.' + p.field
            if kind == 'count':
                self.w('    if d.u64() != 0 { %s = Some(d.u32()); }' % f)
            elif kind == 'handle':
                self.w('    if d.u64() != 0 { %s = Some(d.u64()); }' % f)
            elif kind == 'struct':
                self.w('    if d.u64() != 0 { %s = Some(%s::decode(d)); }' % (f, t))
            elif kind == 'scalar':
                self.w('    if d.u64() != 0 { %s = Some(d.%s()); }' % (f, self.scalar(t)[2]))
            elif kind == 'handles':
                self.w('    let count = d.array_size(4096);')
                self.w('    for _ in 0..count { %s.push(d.u64()); }' % f)
            elif kind == 'structs':
                self.w('    let count = d.array_size(4096);')
                self.w('    for _ in 0..count { %s.push(%s::decode(d)); }' % (f, t))
        self.w('    (!d.error()).then_some(reply)')
        self.w('}')
        self.w()

    # ---- file ------------------------------------------------------------------

    def run(self):
        self.collect()
        w = self.w
        w('// Generated by tools/gen-venus.py from the Vulkan registry -- do not edit.')
        w('// venus-protocol %s' % VENUS_PROTOCOL_COMMIT)
        w('//')
        w('// Regenerate with `make venus` after changing the command list there.')
        w()
        w('//! Venus encoder and reply decoders, generated (see tools/gen-venus.py).')
        w('//!')
        w('//! Each input structure has an `encode` that writes it as the host\'s')
        w('//! decoder reads it; each command has a function writing it whole, and,')
        w('//! when it returns something, a `reply_*` reading its answer. Floating')
        w('//! point values are held as their bit patterns: the kernel is soft-float.')
        w()
        w('#![allow(clippy::all, dead_code, unused_variables, non_snake_case, non_upper_case_globals)]')
        w()
        w('use super::cs::{Dec, Enc};')
        w()
        w('/// VK_COMMAND_GENERATE_REPLY_BIT_EXT.')
        w('pub const GENERATE_REPLY: u32 = 1;')
        w()
        for name in sorted(self.reg.constants):
            if name.startswith('VK_MAX_') or name.startswith('VK_UUID') or name.startswith('VK_LUID'):
                w('pub const %s: u32 = %d;' % (name, self.reg.constants[name]))
        w()
        w('/// Values of the Vulkan enumerations the kernel names.')
        w('pub mod vk {')
        for enum in ENUMS:
            element = self.reg.types.get(enum)
            wide = False
            if enum.endswith('FlagBits2'):
                wide = True
            names = sorted((v, n) for n, v in self.reg.enum_values.items() if self.reg.enum_type.get(n) == enum)
            if not names:
                fail('enumeration vide : %s' % enum)
            w('    // %s' % enum)
            for value, name in names:
                if wide:
                    w('    pub const %s: u64 = 0x%x;' % (name, value))
                elif enum.endswith('FlagBits'):
                    w('    pub const %s: u32 = 0x%x;' % (name, value & 0xFFFFFFFF))
                else:
                    w('    pub const %s: i32 = %d;' % (name, value))
        w('}')
        w()
        w('/// Structure types of the structures generated.')
        w('pub mod stype {')
        for name in sorted(set(self.inputs) | set(self.outputs)):
            st = self.reg.stype(name)
            if st is not None:
                w('    pub const %s: i32 = %d;' % (re.sub(r'([a-z0-9])([A-Z])', r'\1_\2', name[2:]).upper(), st))
        w('}')
        w()
        self.emit_chain()
        for name in self.inputs:
            self.emit_input_struct(name)
        for name in self.outputs:
            self.emit_output_struct(name)
        for command in COMMANDS:
            self.emit_command(command)
        return '\n'.join(self.out) + '\n'


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument('--xml', type=Path, default=Path('build/venus/xml'))
    parser.add_argument('--out', type=Path, default=Path('src/gpu/venus/protocol.rs'))
    args = parser.parse_args()
    for name, want in XML_SUMS.items():
        path = args.xml / name
        if not path.exists():
            fail('%s absent (make venus le telecharge)' % path)
        have = hashlib.sha256(path.read_bytes()).hexdigest()
        if have != want:
            fail('somme de controle fausse : %s' % path)
    reg = Registry(args.xml)
    text = Gen(reg).run()
    args.out.parent.mkdir(parents=True, exist_ok=True)
    args.out.write_text(text)
    print('[ok] encodeur Venus : %s (%d lignes, %d commandes)' % (args.out, text.count('\n'), len(COMMANDS)))


if __name__ == '__main__':
    main()
