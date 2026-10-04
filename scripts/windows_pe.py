"""Read regular and delay-load DLL names from AMD64 PE32+ files.

This is an import-name reader, not a complete Windows loader validator. It
requires file-backed import directories and names, and never reads thunk tables.
Safety limits: 256 MiB files, 96 sections, 4096 descriptors per directory, and
4096 bytes per DLL name. Files outside those limits raise ValueError.
"""

import struct
from pathlib import Path

_MAX_FILE_SIZE = 256 * 1024 * 1024
_MAX_DESCRIPTORS = 4096
_MAX_NAME = 4096


def imported_dlls(path: Path) -> set[str]:
    """Return lowercase ASCII DLL names, including delay-load dependencies.

    Malformed/unsupported PE data raises descriptive ValueError; filesystem
    errors propagate as OSError. Legacy VA-based delay descriptors are accepted
    when their 32-bit name address can represent an address in this image.
    """
    with path.open("rb") as stream:
        if stream.seek(0, 2) > _MAX_FILE_SIZE:
            raise ValueError("PE file exceeds the 256 MiB size limit")
        stream.seek(0)
        data = stream.read(_MAX_FILE_SIZE + 1)
    if len(data) > _MAX_FILE_SIZE:
        raise ValueError("PE file exceeds the 256 MiB size limit")

    def require(offset: int, size: int, label: str) -> None:
        if offset < 0 or size < 0 or offset + size > len(data):
            raise ValueError(f"{label}: out-of-range or truncated file data")

    def unpack(fmt: str, offset: int, label: str) -> tuple:
        require(offset, struct.calcsize(fmt), label)
        return struct.unpack_from(fmt, data, offset)

    require(0, 64, "DOS header")
    if data[:2] != b"MZ":
        raise ValueError("DOS header: missing MZ signature")
    pe_offset = unpack("<I", 0x3C, "PE header offset")[0]
    if pe_offset < 64:
        raise ValueError("PE header offset overlaps DOS header")
    require(pe_offset, 24, "PE/COFF header")
    if data[pe_offset : pe_offset + 4] != b"PE\0\0":
        raise ValueError("PE header: missing PE signature")
    machine, section_count = unpack("<HH", pe_offset + 4, "COFF header")
    if machine != 0x8664:
        raise ValueError(
            f"unsupported PE architecture 0x{machine:04x}; expected x86_64"
        )
    if not 1 <= section_count <= 96:
        raise ValueError("COFF header: section count must be between 1 and 96")
    optional_size = unpack("<H", pe_offset + 20, "optional header size")[0]
    optional = pe_offset + 24
    require(optional, optional_size, "optional header")
    if optional_size < 112:
        raise ValueError("PE32+ optional header is too short")
    if unpack("<H", optional, "optional header magic")[0] != 0x20B:
        raise ValueError("optional header: expected PE32+ magic 0x020b")
    image_base = unpack("<Q", optional + 24, "image base")[0]
    header_size = unpack("<I", optional + 60, "SizeOfHeaders")[0]
    directory_count = unpack("<I", optional + 108, "directory count")[0]
    if directory_count > (optional_size - 112) // 8:
        raise ValueError("data directory count exceeds optional header size")

    section_table = optional + optional_size
    require(section_table, section_count * 40, "section table")
    if header_size < section_table + section_count * 40:
        raise ValueError("SizeOfHeaders does not cover the section table")
    require(0, header_size, "SizeOfHeaders")
    sections = []
    for index in range(section_count):
        virtual_size, rva, raw_size, raw_offset = unpack(
            "<IIII", section_table + index * 40 + 8, f"section {index}"
        )
        extent = max(virtual_size, raw_size)
        if rva + extent > 0x100000000:
            raise ValueError(f"section {index}: RVA range overflows 32 bits")
        if raw_size:
            require(raw_offset, raw_size, f"section {index} raw data")
            if raw_offset < header_size:
                raise ValueError(f"section {index}: raw data overlaps headers")
        if extent:
            if rva < header_size or any(
                rva < start + length and start < rva + extent
                for start, length, _, _ in sections
            ):
                raise ValueError(f"section {index}: overlapping RVA ranges")
            sections.append((rva, extent, raw_offset, raw_size))

    def mapped(rva: int, size: int, label: str) -> tuple[int, int]:
        """Return file offset and remaining bytes in its file-backed region."""
        if not 0 <= rva < 0x100000000 or rva + size > 0x100000000:
            raise ValueError(f"{label}: RVA out of range")
        if rva < header_size:
            if size <= header_size - rva:
                return rva, header_size - rva
        else:
            for start, extent, raw_offset, raw_size in sections:
                if start <= rva < start + extent:
                    delta = rva - start
                    if delta + size <= raw_size:
                        return raw_offset + delta, raw_size - delta
                    break
        raise ValueError(f"{label}: RVA 0x{rva:x} is not fully file-backed")

    def dll_name(rva: int, label: str) -> str:
        if not rva:
            raise ValueError(f"{label}: missing DLL name RVA")
        offset, available = mapped(rva, 1, label)
        end = data.find(b"\0", offset, offset + min(available, _MAX_NAME + 1))
        if end == -1:
            raise ValueError(
                f"{label}: unterminated DLL name or name exceeds 4096 bytes"
            )
        name = data[offset:end]
        if not name or any(byte < 32 or byte >= 127 for byte in name):
            raise ValueError(f"{label}: DLL name must be nonempty printable ASCII")
        return name.decode("ascii").lower()

    result = set()
    for directory_index, width, label in (
        (1, 20, "import directory"),
        (13, 32, "delay-import directory"),
    ):
        if directory_index >= directory_count:
            continue
        rva, size = unpack("<II", optional + 112 + directory_index * 8, label)
        if rva == 0 and size == 0:
            continue
        if not rva or size < width:
            raise ValueError(f"{label}: invalid RVA or size")
        offset, _ = mapped(rva, size, label)
        for index in range(min(size // width, _MAX_DESCRIPTORS)):
            descriptor = unpack("<" + "I" * (width // 4), offset + index * width, label)
            if not any(descriptor):
                break
            if directory_index == 1:
                name_rva = descriptor[3]
            else:
                attributes, name_rva = descriptor[:2]
                if attributes & ~1:
                    raise ValueError(
                        f"{label}: unsupported descriptor attributes 0x{attributes:x}"
                    )
                if not attributes & 1:
                    name_rva -= image_base
            result.add(dll_name(name_rva, f"{label} descriptor {index}"))
        else:
            raise ValueError(
                f"{label}: missing null descriptor or descriptor limit exceeded"
            )
    return result
