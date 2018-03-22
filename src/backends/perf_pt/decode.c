// Copyright (c) 2018 King's College London
// created by the Software Development Team <http://soft-dev.org/>
//
// The Universal Permissive License (UPL), Version 1.0
//
// Subject to the condition set forth below, permission is hereby granted to any
// person obtaining a copy of this software, associated documentation and/or
// data (collectively the "Software"), free of charge and under any and all
// copyright rights in the Software, and any and all patent rights owned or
// freely licensable by each licensor hereunder covering either (i) the
// unmodified Software as contributed to or provided by such licensor, or (ii)
// the Larger Works (as defined below), to deal in both
//
// (a) the Software, and
// (b) any piece of software and/or hardware listed in the lrgrwrks.txt file
// if one is included with the Software (each a "Larger Work" to which the Software
// is contributed by such licensors),
//
// without restriction, including without limitation the rights to copy, create
// derivative works of, display, perform, and distribute the Software and make,
// use, sell, offer for sale, import, export, have made, and have sold the
// Software and the Larger Work(s), and to sublicense the foregoing rights on
// either these or other terms.
//
// This license is subject to the following condition: The above copyright
// notice and either this complete permission notice or at a minimum a reference
// to the UPL must be included in all copies or substantial portions of the
// Software.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR
// IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF MERCHANTABILITY,
// FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO EVENT SHALL THE
// AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER
// LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM,
// OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR OTHER DEALINGS IN THE
// SOFTWARE.

#define _GNU_SOURCE

#include <stdio.h>
#include <intel-pt.h>
#include <pt_cpu.h>
#include <stdbool.h>
#include <inttypes.h>
#include <stdint.h>
#include <link.h>
#include <errno.h>
#include <stdlib.h>
#include <unistd.h>
#include <assert.h>

#include "perf_pt_private.h"

struct load_self_image_args {
    struct pt_image *image;
    int vdso_fd;
    char *vdso_filename;
};

// Private prototypes.
static int drain_events(struct pt_block_decoder *, int);
static bool load_self_image(struct load_self_image_args *);
static int load_self_image_cb(struct dl_phdr_info *, size_t, void *);

// Public prototypes.
void *perf_pt_init_block_decoder(void *, uint64_t, int, char *, int *);
bool perf_pt_next_block(struct pt_block_decoder *, int *, uint64_t *);
void perf_pt_free_block_decoder(struct pt_block_decoder *);

/*
 * Get ready to retrieve the basic blocks from a PT trace using the code of the
 * current process for control flow recovery.
 *
 * Accepts a raw buffer `buf` of length `len`.
 *
 * `vdso_fd` is an open file descriptor for the filename `vdso_filename`. This
 * is where the VDSO code will be written. libipt will read this file lazily,
 * so it's up to the caller to make sure this file lives long enough for their
 * purposes.
 *
 * `*decoder_status` will be updated to reflect the status of the decoder after
 * it has been synchronised.
 *
 * Returns a pointer to a configured libipt block decoder or NULL on error.
 */
void *
perf_pt_init_block_decoder(void *buf, uint64_t len, int vdso_fd, char *vdso_filename,
                           int *decoder_status) {
    bool failing = false;

    // Make a block decoder configuration.
    struct pt_config config;
    memset(&config, 0, sizeof(config));
    config.size = sizeof(config);
    config.begin = buf;
    config.end = buf + len;
    config.flags.variant.block.end_on_call = 1;
    config.flags.variant.block.end_on_jump = 1;

    // Decode for the current CPU.
    struct pt_block_decoder *decoder = NULL;
    if (pt_cpu_read(&config.cpu) != 0) {
        failing = true;
        goto clean;
    }

    // Work around CPU bugs.
    if ((config.cpu.vendor) && (pt_cpu_errata(&config.errata, &config.cpu) < 0)) {
        failing = true;
        goto clean;
    }

    // Instantiate a decoder.
    decoder = pt_blk_alloc_decoder(&config);
    if (decoder == NULL) {
        failing = true;
        goto clean;
    }

    // Sync the decoder.
    *decoder_status = pt_blk_sync_forward(decoder);
    if (*decoder_status == -pte_eos) {
        // There were no blocks in the stream. The user will find out on next
        // call to perf_pt_next_block().
        goto clean;
    } else if (*decoder_status < 0) {
        failing = true;
        goto clean;
    }

    // Build and load a memory image from which to recover control flow.
    struct pt_image *image = pt_image_alloc(NULL);
    if (image == NULL) {
        failing = true;
        goto clean;
    }

    struct load_self_image_args load_args = {image, vdso_fd, vdso_filename};
    if (!load_self_image(&load_args)) {
        failing = true;
        goto clean;
    }

    if (pt_blk_set_image(decoder, image) < 0) {
        failing = true;
        goto clean;
    }

clean:
    if (failing) {
        pt_blk_free_decoder(decoder);
        return NULL;
    }
    return decoder;
}

/*
 * Updates `*addr' with the start address of the next block in the instruction
 * stream.
 *
 * A start address of 0 indicates that the end of the instruction stream has
 * been reached.
 *
 * `*decoder_status` will be updated with the new decoder status after the operation.
 *
 * Returns true on success or false otherwise. Upon failure, the start address
 * is undefined.
 */
bool
perf_pt_next_block(struct pt_block_decoder *decoder, int *decoder_status,
                   uint64_t *addr) {
    // If there are events pending, drain them first.
    *decoder_status = drain_events(decoder, *decoder_status);
    if (*decoder_status < 0) {
        // Error.
        return false;
    } else if (*decoder_status & pts_eos) {
        // End of stream.
        *addr = 0;
        return true;
    }
    assert(*decoder_status == 0);

    // Now fetch the block information.
    struct pt_block block;
    *decoder_status = pt_blk_next(decoder, &block, sizeof(block));
    // Other +ve decoder status codes can arise here. We ignore them for now,
    // and let them be detected by drain_events() above when we are next
    // called.
    if (*decoder_status < 0) {
        if (*decoder_status == -pte_eos) {
            // End of stream is flagged as an error in the case of pt_blk_next().
            *addr = 0;
            return true;
        } else {
            // A real error.
            return false;
        }
    }
    // It's possible at this point that we get notified of an event in the
    // stream. This will be handled in the next call to `perf_pt_next_block`.
    assert((*decoder_status == 0) || (*decoder_status == pts_event_pending));

    // XXX A truncated block occurs when a block straddles a section boundary.
    // In this case we may need some extra logic, but this should be rare.
    assert(block.truncated == 0);

    // A block should have at least one instruction.
    assert(block.ninsn > 0);

    *addr = block.ip;
    return true;
}

/*
 * Given a decoder and the last decoder status, drains events in the PT packet
 * stream.
 *
 * The new decoder status is returned.
 */
static int
drain_events(struct pt_block_decoder *decoder, int decoder_status) {
    while(decoder_status & pts_event_pending) {
        struct pt_event event;
        decoder_status = pt_blk_event(decoder, &event, sizeof(event));
        if (decoder_status < 0) {
            return decoder_status;
        }
    }
    return decoder_status;
}

/*
 * Loads the libipt image `image` with the code of the current process.
 *
 * Returns true on success or false otherwise.
 */
static bool
load_self_image(struct load_self_image_args *args)
{
    return ((dl_iterate_phdr(load_self_image_cb, args) == 0) &&
            (fsync(args->vdso_fd) == 0));
}

/*
 * The callback for `load_self_image()`, called once for each program header.
 *
 * Returns 1 to stop iterating, and in our case to indicate an error. Returns 0
 * on success and to continue iterating. See dl_iterate_phdr(3) for information
 * on this interface.
 */
static int
load_self_image_cb(struct dl_phdr_info *info, size_t size, void *data)
{
    ElfW(Phdr) phdr;
    ElfW(Half) i;

    (void) size; // Unused. Silence warning.
    struct load_self_image_args *args = data;

    const char *filename = info->dlpi_name;
    bool vdso = false;
    if (!*filename) {
        // On Linux, an empty name means that it is the executable itself.
        filename = program_invocation_name;
    } else {
        vdso = strcmp(filename, VDSO_NAME) == 0;
    }

    for (i = 0; i < info->dlpi_phnum; i++) {
        phdr = info->dlpi_phdr[i];
        // Ensure we only use loadable and executable sections.
        if ((phdr.p_type != PT_LOAD) || (!(phdr.p_flags & PF_X))) {
            continue;
        }

        uint64_t vaddr = info->dlpi_addr + phdr.p_vaddr;
        uint64_t offset;

        // Load the code into the libipt image.
        //
        // The VDSO is special. It doesn't exist on-disk as a regular library,
        // but rather it is a set of pages shared with the kernel.
        //
        // XXX Since libipt currently requires us to load from a file, we have
        // to dump the VDSO to disk and have libipt load it back in.
        //
        // Discussion on adding libipt support for loading from memory here:
        // https://github.com/01org/processor-trace/issues/37
        if (vdso) {
            int rv = dump_vdso(args->vdso_fd, vaddr, phdr.p_filesz);
            if (!rv) {
                return 1;
            }
            filename = args->vdso_filename;
            offset = 0;
        } else {
            offset = phdr.p_offset;
        }

        // XXX This could be made faster using a libipt instruction cache.
        int rv = pt_image_add_file(args->image, filename, offset,
                                   phdr.p_filesz, NULL, vaddr);
        if (rv < 0) {
            return 1;
        }
    }

    return 0;
}

/*
 * Dump the VDSO code into the open file descriptor `fd`, starting at `vaddr`
 * and of size `len` into a temp file.
 *
 * Returns true on success or false otherwise.
 */
bool
dump_vdso(int fd, uint64_t vaddr, size_t len)
{
    size_t written = 0;
    while (written != len) {
        int wrote = write(fd, (void *) vaddr + written, len - written);
        if (wrote == -1) {
            return false;
        }
        written += wrote;
    }

    return true;
}

/*
 * Free a block decoder and its image.
 */
void
perf_pt_free_block_decoder(struct pt_block_decoder *decoder) {
    if (decoder != NULL) {
        struct pt_image *img = pt_blk_get_image(decoder);
        if (img != NULL) {
            pt_image_free(img);
        }
    }
}
