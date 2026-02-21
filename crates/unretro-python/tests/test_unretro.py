"""
Comprehensive tests for unretro Python bindings.

Tests cover:
- Loader iteration
- Entry IO interface
- open(), walk(), listdir() functions
- Inline test archives (ZIP, TAR, GZIP)
- Real test files from testdata
"""

import io
import os
import gzip
import tarfile
import tempfile
import zipfile
from pathlib import Path

import pytest

import unretro


# =============================================================================
# Test Fixtures - Inline Archives
# =============================================================================

@pytest.fixture
def simple_zip_bytes():
    """Create a simple ZIP archive in memory."""
    buf = io.BytesIO()
    with zipfile.ZipFile(buf, 'w', zipfile.ZIP_DEFLATED) as zf:
        zf.writestr('hello.txt', b'Hello, World!')
        zf.writestr('data.bin', bytes(range(256)))
        zf.writestr('folder/nested.txt', b'Nested file content')
    return buf.getvalue()


@pytest.fixture
def simple_tar_bytes():
    """Create a simple TAR archive in memory."""
    buf = io.BytesIO()
    with tarfile.open(fileobj=buf, mode='w') as tf:
        # Add a text file
        data = b'TAR file content here'
        info = tarfile.TarInfo(name='readme.txt')
        info.size = len(data)
        tf.addfile(info, io.BytesIO(data))

        # Add binary data
        binary = bytes(range(128))
        info = tarfile.TarInfo(name='binary.dat')
        info.size = len(binary)
        tf.addfile(info, io.BytesIO(binary))
    return buf.getvalue()


@pytest.fixture
def simple_gzip_bytes():
    """Create a GZIP compressed file in memory."""
    original = b'This is the original uncompressed content. ' * 10
    return gzip.compress(original)


@pytest.fixture
def temp_zip_file(simple_zip_bytes, tmp_path):
    """Write ZIP to a temporary file."""
    path = tmp_path / "test.zip"
    path.write_bytes(simple_zip_bytes)
    return str(path)


@pytest.fixture
def temp_tar_file(simple_tar_bytes, tmp_path):
    """Write TAR to a temporary file."""
    path = tmp_path / "test.tar"
    path.write_bytes(simple_tar_bytes)
    return str(path)


@pytest.fixture
def temp_gzip_file(simple_gzip_bytes, tmp_path):
    """Write GZIP to a temporary file."""
    path = tmp_path / "test.txt.gz"
    path.write_bytes(simple_gzip_bytes)
    return str(path)


# Path to real test files (relative to this test file)
# tests/test_unretro.py is at crates/unretro-python/tests/test_unretro.py
# parent chain: tests -> unretro-python -> crates -> repo-root
REPO_ROOT = Path(__file__).parent.parent.parent.parent
TESTDATA_DIR = REPO_ROOT.parent / "unretro-samples" / "data"
UNRETRO_TESTDATA = REPO_ROOT / "testdata"


def has_testdata():
    """Check if test data directory exists."""
    return TESTDATA_DIR.exists() and TESTDATA_DIR.is_dir()


def has_lha_testdata():
    """Check if LHA test file exists."""
    lha_file = TESTDATA_DIR / "test.lha"
    return lha_file.exists() and lha_file.is_file()


# =============================================================================
# Tests: Loader Class
# =============================================================================

class TestLoader:
    """Tests for unretro.Loader class."""

    def test_loader_from_path_zip(self, temp_zip_file):
        """Test loading a ZIP file from path."""
        loader = unretro.Loader(path=temp_zip_file)
        assert repr(loader).startswith("Loader(path=")

    def test_loader_from_bytes_zip(self, simple_zip_bytes):
        """Test loading a ZIP from bytes."""
        loader = unretro.Loader(data=simple_zip_bytes, name="test.zip")
        assert repr(loader).startswith("Loader(data=")

    def test_loader_iteration_zip(self, temp_zip_file):
        """Test iterating over ZIP entries."""
        entries = list(unretro.Loader(path=temp_zip_file))
        assert len(entries) == 3

        names = {e.name for e in entries}
        assert "hello.txt" in names
        assert "data.bin" in names
        assert "nested.txt" in names

    def test_loader_iteration_tar(self, temp_tar_file):
        """Test iterating over TAR entries."""
        entries = list(unretro.Loader(path=temp_tar_file))
        assert len(entries) == 2

        names = {e.name for e in entries}
        assert "readme.txt" in names
        assert "binary.dat" in names

    def test_loader_from_bytes_tar(self, simple_tar_bytes):
        """Test loading TAR from bytes."""
        entries = list(unretro.Loader(data=simple_tar_bytes, name="test.tar"))
        assert len(entries) == 2

    def test_loader_max_depth(self, temp_zip_file):
        """Test max_depth setting."""
        loader = unretro.Loader(path=temp_zip_file).with_max_depth(1)
        entries = list(loader)
        # Should still work with depth 1
        assert len(entries) >= 1

    def test_loader_filter_extension(self, temp_zip_file):
        """Test extension filtering."""
        loader = unretro.Loader(path=temp_zip_file).filter_extension(["txt"])
        entries = list(loader)

        # Should only get .txt files
        for e in entries:
            assert e.extension == "txt"

    def test_loader_filter_path(self, temp_zip_file):
        """Test path prefix filtering."""
        loader = unretro.Loader(path=temp_zip_file).filter_path("folder")
        entries = list(loader)

        # May or may not have results depending on path structure
        for e in entries:
            assert "folder" in e.path

    def test_loader_requires_path_or_data(self):
        """Test that Loader requires either path or data."""
        with pytest.raises(ValueError, match="Either path or data"):
            unretro.Loader()

    def test_loader_data_requires_name(self, simple_zip_bytes):
        """Test that loading from data requires name."""
        with pytest.raises(ValueError, match="name is required"):
            unretro.Loader(data=simple_zip_bytes)

    def test_loader_cannot_have_both_path_and_data(self, simple_zip_bytes, temp_zip_file):
        """Test that both path and data cannot be specified."""
        with pytest.raises(ValueError, match="Cannot specify both"):
            unretro.Loader(path=temp_zip_file, data=simple_zip_bytes, name="test.zip")


# =============================================================================
# Tests: Entry Class
# =============================================================================

class TestEntry:
    """Tests for unretro.Entry class."""

    def test_entry_properties(self, temp_zip_file):
        """Test entry properties."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                assert entry.size == 13  # "Hello, World!"
                assert entry.extension == "txt"
                assert "hello.txt" in entry.path
                break

    def test_entry_read_all(self, temp_zip_file):
        """Test reading all data from entry."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                data = entry.read()
                assert data == b"Hello, World!"
                break

    def test_entry_read_partial(self, temp_zip_file):
        """Test partial reads."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                chunk1 = entry.read(5)
                chunk2 = entry.read(5)
                chunk3 = entry.read()  # Rest

                assert chunk1 == b"Hello"
                assert chunk2 == b", Wor"
                assert chunk3 == b"ld!"
                break

    def test_entry_seek_tell(self, temp_zip_file):
        """Test seek and tell."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                assert entry.tell() == 0

                entry.read(5)
                assert entry.tell() == 5

                entry.seek(0)
                assert entry.tell() == 0

                entry.seek(0, 2)  # End
                assert entry.tell() == 13
                break

    def test_entry_seek_whence(self, temp_zip_file):
        """Test seek with different whence values."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                # SEEK_SET (0)
                entry.seek(5, 0)
                assert entry.tell() == 5

                # SEEK_CUR (1)
                entry.seek(2, 1)
                assert entry.tell() == 7

                # SEEK_END (2)
                entry.seek(-3, 2)
                assert entry.tell() == 10
                break

    def test_entry_len(self, temp_zip_file):
        """Test __len__."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "hello.txt":
                assert len(entry) == 13
                break

    def test_entry_io_methods(self, temp_zip_file):
        """Test IO interface methods."""
        for entry in unretro.Loader(path=temp_zip_file):
            assert entry.readable() is True
            assert entry.writable() is False
            assert entry.seekable() is True
            break

    def test_entry_binary_data(self, temp_zip_file):
        """Test reading binary data."""
        for entry in unretro.Loader(path=temp_zip_file):
            if entry.name == "data.bin":
                data = entry.read()
                assert len(data) == 256
                assert data == bytes(range(256))
                break

    def test_entry_repr(self, temp_zip_file):
        """Test entry __repr__."""
        for entry in unretro.Loader(path=temp_zip_file):
            r = repr(entry)
            assert "Entry(" in r
            assert "path=" in r
            assert "size=" in r
            break


# =============================================================================
# Tests: open() Function
# =============================================================================

class TestOpen:
    """Tests for unretro.open() function."""

    def test_open_basic(self, temp_zip_file):
        """Test basic open usage."""
        # Note: open returns the first entry in the archive when given archive path
        with unretro.open(temp_zip_file) as f:
            data = f.read()
            assert len(data) > 0

    def test_open_context_manager(self, temp_zip_file):
        """Test open as context manager."""
        with unretro.open(temp_zip_file) as f:
            assert f.readable()
            data = f.read()
            assert len(data) > 0
        # Entry should be closed after context

    def test_open_explicit_close(self, temp_zip_file):
        """Test explicit close."""
        f = unretro.open(temp_zip_file)
        data = f.read()
        assert len(data) > 0
        f.close()

    def test_open_not_found(self, tmp_path):
        """Test opening non-existent file."""
        with pytest.raises(FileNotFoundError):
            unretro.open(str(tmp_path / "nonexistent.zip"))

    def test_open_max_depth(self, temp_zip_file):
        """Test open with max_depth parameter."""
        with unretro.open(temp_zip_file, max_depth=1) as f:
            data = f.read()
            assert len(data) > 0


# =============================================================================
# Tests: walk() Function
# =============================================================================

class TestWalk:
    """Tests for unretro.walk() function."""

    def test_walk_basic(self, temp_zip_file):
        """Test basic walk usage."""
        results = list(unretro.walk(temp_zip_file))
        assert len(results) >= 1

    def test_walk_result_unpacking(self, temp_zip_file):
        """Test that WalkResult can be unpacked."""
        for dirpath, dirnames, filenames in unretro.walk(temp_zip_file):
            assert isinstance(dirpath, str)
            assert isinstance(dirnames, list)
            assert isinstance(filenames, list)

    def test_walk_result_properties(self, temp_zip_file):
        """Test WalkResult properties."""
        for result in unretro.walk(temp_zip_file):
            assert hasattr(result, 'dirpath')
            assert hasattr(result, 'dirnames')
            assert hasattr(result, 'filenames')
            break

    def test_walk_result_indexing(self, temp_zip_file):
        """Test WalkResult indexing."""
        for result in unretro.walk(temp_zip_file):
            assert result[0] == result.dirpath
            assert result[1] == result.dirnames
            assert result[2] == result.filenames
            assert result[-1] == result.filenames  # Negative indexing
            break

    def test_walk_topdown_true(self, temp_zip_file):
        """Test walk with topdown=True (default)."""
        results = list(unretro.walk(temp_zip_file, topdown=True))
        assert len(results) >= 1

    def test_walk_topdown_false(self, temp_zip_file):
        """Test walk with topdown=False."""
        results = list(unretro.walk(temp_zip_file, topdown=False))
        assert len(results) >= 1

    def test_walk_max_depth(self, temp_zip_file):
        """Test walk with max_depth."""
        results = list(unretro.walk(temp_zip_file, max_depth=1))
        assert len(results) >= 1

    def test_walk_invalid_path_raises(self, tmp_path):
        """Test walk on a missing path raises RuntimeError when iterated."""
        iterator = unretro.walk(str(tmp_path / "missing.zip"))
        with pytest.raises(RuntimeError):
            next(iterator)


# =============================================================================
# Tests: listdir() Function
# =============================================================================

class TestListdir:
    """Tests for unretro.listdir() function."""

    def test_listdir_basic(self, temp_zip_file):
        """Test basic listdir usage."""
        entries = unretro.listdir(temp_zip_file)
        assert isinstance(entries, list)
        assert len(entries) >= 1

    def test_listdir_returns_names(self, temp_zip_file):
        """Test that listdir returns names, not full paths."""
        entries = unretro.listdir(temp_zip_file)
        for name in entries:
            assert isinstance(name, str)
            assert "/" not in name or name.count("/") == 0  # Just names

    def test_listdir_invalid_path_raises(self, tmp_path):
        """Test listdir on a missing path raises RuntimeError."""
        with pytest.raises(RuntimeError):
            unretro.listdir(str(tmp_path / "missing.zip"))


# =============================================================================
# Tests: detect_format() Function
# =============================================================================

class TestDetectFormat:
    """Tests for unretro.detect_format() function."""

    def test_detect_zip(self, temp_zip_file):
        """Test detecting ZIP format from file content."""
        fmt = unretro.detect_format(temp_zip_file)
        assert fmt is not None
        assert "ZIP" in fmt.name or "Zip" in fmt.name

    def test_detect_tar(self, temp_tar_file):
        """Test detecting TAR format from file content."""
        fmt = unretro.detect_format(temp_tar_file)
        assert fmt is not None
        assert "TAR" in fmt.name or "Tar" in fmt.name

    def test_detect_gzip(self, temp_gzip_file):
        """Test detecting GZIP format from file content."""
        fmt = unretro.detect_format(temp_gzip_file)
        assert fmt is not None
        assert "Gzip" in fmt.name or "GZIP" in fmt.name

    def test_detect_unknown(self, tmp_path):
        """Test detecting unknown format from file content."""
        unknown = tmp_path / "file.unknown"
        unknown.write_bytes(b"not a known container")
        fmt = unretro.detect_format(str(unknown))
        assert fmt is None

    def test_detect_nonexistent_path(self, tmp_path):
        """Test detecting a missing file path."""
        fmt = unretro.detect_format(str(tmp_path / "missing.zip"))
        assert fmt is None


# =============================================================================
# Tests: ContainerFormat Class
# =============================================================================

class TestContainerFormat:
    """Tests for unretro.ContainerFormat class."""

    def test_format_properties(self, temp_zip_file):
        """Test ContainerFormat properties."""
        fmt = unretro.detect_format(temp_zip_file)
        assert fmt is not None
        assert isinstance(fmt.name, str)
        assert isinstance(fmt.is_multi_file, bool)
        assert fmt.is_multi_file is True  # ZIP can hold multiple files


# =============================================================================
# Tests: Real Test Files (when available)
# =============================================================================

@pytest.mark.skipif(not has_testdata(), reason="Test data not available")
class TestRealFiles:
    """Tests using real test files from testdata directory."""

    @pytest.mark.skipif(not has_lha_testdata(), reason="LHA test file not available")
    def test_lha_file(self):
        """Test loading LHA archive."""
        lha_path = str(TESTDATA_DIR / "test.lha")
        count = 0
        for entry in unretro.Loader(path=lha_path):
            # Read data within the loop (data only valid during current iteration)
            data = entry.read()
            assert len(data) == entry.size
            count += 1
        assert count >= 1

    @pytest.mark.skipif(not (TESTDATA_DIR / "DEADLOCK.XM.gz").exists(),
                        reason="GZIP test file not available")
    def test_gzip_file(self):
        """Test loading GZIP file."""
        gz_path = str(TESTDATA_DIR / "DEADLOCK.XM.gz")
        with unretro.open(gz_path) as f:
            data = f.read()
            assert len(data) > 0

    @pytest.mark.skipif(not (TESTDATA_DIR / "DOOM.WAD").exists(),
                        reason="WAD test file not available")
    def test_wad_file(self):
        """Test loading WAD game file."""
        wad_path = str(TESTDATA_DIR / "DOOM.WAD")
        entries = list(unretro.Loader(path=wad_path))
        assert len(entries) > 0


# =============================================================================
# Tests: Edge Cases and Error Handling
# =============================================================================

class TestEdgeCases:
    """Tests for edge cases and error handling."""

    def test_empty_archive(self, tmp_path):
        """Test handling of empty archive."""
        # Create empty ZIP
        zip_path = tmp_path / "empty.zip"
        with zipfile.ZipFile(zip_path, 'w') as zf:
            pass  # Empty archive

        entries = list(unretro.Loader(path=str(zip_path)))
        assert len(entries) == 0

    def test_large_file_in_archive(self, tmp_path):
        """Test handling of larger files."""
        # Create ZIP with larger file
        zip_path = tmp_path / "large.zip"
        large_data = b"X" * (1024 * 1024)  # 1MB

        with zipfile.ZipFile(zip_path, 'w', zipfile.ZIP_DEFLATED) as zf:
            zf.writestr('large.bin', large_data)

        with unretro.open(str(zip_path)) as f:
            data = f.read()
            assert len(data) == len(large_data)
            assert data == large_data

    def test_unicode_filename(self, tmp_path):
        """Test handling of unicode filenames."""
        zip_path = tmp_path / "unicode.zip"

        with zipfile.ZipFile(zip_path, 'w') as zf:
            zf.writestr('cafè_日本語.txt', b'Unicode content')

        entries = list(unretro.Loader(path=str(zip_path)))
        assert len(entries) == 1

    def test_multiple_iterations(self, temp_zip_file):
        """Test that loader can be iterated multiple times."""
        loader = unretro.Loader(path=temp_zip_file)

        entries1 = list(loader)
        entries2 = list(loader)

        assert len(entries1) == len(entries2)

    def test_entry_after_iteration(self, temp_zip_file):
        """Test that entries are invalid after iterator advances."""
        entries = []
        for entry in unretro.Loader(path=temp_zip_file):
            entries.append(entry)

        # Entries should NOT be readable after the loop (data is released)
        for entry in entries:
            with pytest.raises(RuntimeError, match="no longer available"):
                entry.read()


# =============================================================================
# Tests: Version and Module
# =============================================================================

class TestModule:
    """Tests for module-level attributes."""

    def test_version(self):
        """Test __version__ attribute."""
        assert hasattr(unretro, '__version__')
        assert isinstance(unretro.__version__, str)
        assert len(unretro.__version__) > 0

    def test_all_exports(self):
        """Test that __all__ exports are accessible."""
        for name in unretro.__all__:
            assert hasattr(unretro, name), f"Missing export: {name}"


if __name__ == "__main__":
    pytest.main([__file__, "-v"])
