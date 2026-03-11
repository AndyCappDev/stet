#!/usr/bin/env python3
"""Post-process a WASM binary to add legacy TLS exports for wasm-bindgen.

Newer LLVM (used in Rust nightly 2025+) renamed the WASM TLS symbols:
  __wasm_init_tls → __wasm_apply_tls_relocs
  __tls_size / __tls_align → removed (handled internally)
  __tls_base → still exists but not exported

wasm-bindgen 0.2.114 still looks for the old names. This script:
1. Finds __wasm_apply_tls_relocs in the name section → exports as __wasm_init_tls
2. Finds __tls_base global in the name section → exports as __tls_base
3. Adds __tls_size (i32 = 0) and __tls_align (i32 = 1) as global exports
"""

import sys


def read_leb128(data, pos):
    result = 0
    shift = 0
    while True:
        b = data[pos]
        result |= (b & 0x7F) << shift
        pos += 1
        if (b & 0x80) == 0:
            break
        shift += 7
    return result, pos


def encode_leb128(value):
    result = bytearray()
    while True:
        byte = value & 0x7F
        value >>= 7
        if value != 0:
            byte |= 0x80
        result.append(byte)
        if value == 0:
            break
    return bytes(result)


def encode_str(s):
    b = s.encode('utf-8')
    return encode_leb128(len(b)) + b


def find_names(data):
    """Find function and global names from the custom 'name' section."""
    func_names = {}   # name → func_idx
    global_names = {}  # name → global_idx
    pos = 8
    while pos < len(data):
        sid = data[pos]
        pos += 1
        sz, pos = read_leb128(data, pos)
        end = pos + sz
        if sid == 0:  # Custom section
            nl, inner = read_leb128(data, pos)
            name = data[inner:inner + nl].decode('utf-8', errors='replace')
            inner += nl
            if name == 'name':
                while inner < end:
                    subsec_id = data[inner]
                    inner += 1
                    subsec_sz, inner = read_leb128(data, inner)
                    subsec_end = inner + subsec_sz
                    if subsec_id == 1:  # Function names
                        cnt, p = read_leb128(data, inner)
                        for _ in range(cnt):
                            idx, p = read_leb128(data, p)
                            nl2, p = read_leb128(data, p)
                            fname = data[p:p + nl2].decode('utf-8', errors='replace')
                            p += nl2
                            func_names[fname] = idx
                    elif subsec_id == 7:  # Global names
                        cnt, p = read_leb128(data, inner)
                        for _ in range(cnt):
                            idx, p = read_leb128(data, p)
                            nl2, p = read_leb128(data, p)
                            gname = data[p:p + nl2].decode('utf-8', errors='replace')
                            p += nl2
                            global_names[gname] = idx
                    inner = subsec_end
        pos = end
    return func_names, global_names


def count_globals(data):
    """Count existing globals (imports + local)."""
    import_globals = 0
    local_globals = 0
    pos = 8
    while pos < len(data):
        sid = data[pos]
        pos += 1
        sz, pos = read_leb128(data, pos)
        end = pos + sz
        if sid == 2:  # Import section
            cnt, inner = read_leb128(data, pos)
            for _ in range(cnt):
                ml, inner = read_leb128(data, inner)
                inner += ml
                fl, inner = read_leb128(data, inner)
                inner += fl
                kind = data[inner]
                inner += 1
                if kind == 0:  # function
                    _, inner = read_leb128(data, inner)
                elif kind == 1:  # table
                    inner += 1  # reftype
                    flags = data[inner]; inner += 1
                    _, inner = read_leb128(data, inner)
                    if flags & 1:
                        _, inner = read_leb128(data, inner)
                elif kind == 2:  # memory
                    flags = data[inner]; inner += 1
                    _, inner = read_leb128(data, inner)
                    if flags & 1:
                        _, inner = read_leb128(data, inner)
                elif kind == 3:  # global
                    inner += 2  # valtype + mutability
                    import_globals += 1
        elif sid == 6:  # Global section
            cnt, _ = read_leb128(data, pos)
            local_globals = cnt
        pos = end
    return import_globals, local_globals


def patch_wasm(input_path, output_path):
    data = bytearray(open(input_path, 'rb').read())

    func_names, global_names = find_names(data)

    # Find __wasm_apply_tls_relocs function index
    func_idx = func_names.get('__wasm_apply_tls_relocs')
    if func_idx is None:
        print('WARNING: __wasm_apply_tls_relocs not found, skipping TLS patch')
        open(output_path, 'wb').write(data)
        return

    # Find __tls_base global index
    tls_base_idx = global_names.get('__tls_base')
    if tls_base_idx is None:
        print('WARNING: __tls_base not found, skipping TLS patch')
        open(output_path, 'wb').write(data)
        return

    print(f'Found __wasm_apply_tls_relocs at func[{func_idx}]')
    print(f'Found __tls_base at global[{tls_base_idx}]')

    import_globals, local_globals = count_globals(data)
    total_globals = import_globals + local_globals
    new_global_size_idx = total_globals      # __tls_size
    new_global_align_idx = total_globals + 1  # __tls_align

    # Rebuild the WASM binary with patched Global and Export sections
    result = bytearray()
    result.extend(data[:8])  # magic + version

    pos = 8
    while pos < len(data):
        sid = data[pos]
        pos += 1
        sz, pos = read_leb128(data, pos)
        end = pos + sz
        section_data = data[pos:end]

        if sid == 6:  # Global section — add __tls_size and __tls_align
            cnt, inner_off = read_leb128(section_data, 0)
            remaining = section_data[inner_off:]
            new_cnt = cnt + 2

            new_section = bytearray()
            new_section.extend(encode_leb128(new_cnt))
            new_section.extend(remaining)
            # __tls_size: i32, immutable, i32.const 0, end
            new_section.extend(bytes([0x7F, 0x00, 0x41, 0x00, 0x0B]))
            # __tls_align: i32, immutable, i32.const 1, end
            new_section.extend(bytes([0x7F, 0x00, 0x41, 0x01, 0x0B]))

            result.append(sid)
            result.extend(encode_leb128(len(new_section)))
            result.extend(new_section)

        elif sid == 7:  # Export section — add TLS exports
            cnt, inner_off = read_leb128(section_data, 0)
            remaining = section_data[inner_off:]
            new_cnt = cnt + 4  # __wasm_init_tls + __tls_base + __tls_size + __tls_align

            new_section = bytearray()
            new_section.extend(encode_leb128(new_cnt))
            new_section.extend(remaining)

            # Export __wasm_init_tls → function
            new_section.extend(encode_str('__wasm_init_tls'))
            new_section.append(0x00)  # func export
            new_section.extend(encode_leb128(func_idx))

            # Export __tls_base → global
            new_section.extend(encode_str('__tls_base'))
            new_section.append(0x03)  # global export
            new_section.extend(encode_leb128(tls_base_idx))

            # Export __tls_size → global
            new_section.extend(encode_str('__tls_size'))
            new_section.append(0x03)  # global export
            new_section.extend(encode_leb128(new_global_size_idx))

            # Export __tls_align → global
            new_section.extend(encode_str('__tls_align'))
            new_section.append(0x03)  # global export
            new_section.extend(encode_leb128(new_global_align_idx))

            result.append(sid)
            result.extend(encode_leb128(len(new_section)))
            result.extend(new_section)

        else:
            # Copy section as-is
            result.append(sid)
            result.extend(encode_leb128(sz))
            result.extend(section_data)

        pos = end

    open(output_path, 'wb').write(result)
    print(f'Patched WASM written to {output_path} ({len(result)} bytes)')


if __name__ == '__main__':
    if len(sys.argv) != 3:
        print(f'Usage: {sys.argv[0]} <input.wasm> <output.wasm>')
        sys.exit(1)
    patch_wasm(sys.argv[1], sys.argv[2])
