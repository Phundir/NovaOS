/* The runtime the reference encoder of venus-protocol expects (see
 * vn_protocol_driver_cs.h), reduced to what the check needs: a growing
 * byte buffer. Handles are their object ids, as in the Nova encoder. */
#ifndef VN_CS_H
#define VN_CS_H

#include <stdbool.h>
#include <stdint.h>
#include <stdlib.h>
#include <string.h>
#include <vulkan/vulkan.h>

struct vn_cs_encoder {
   uint8_t *buf;
   size_t len;
   size_t cap;
};

static inline void
vn_cs_encoder_write(struct vn_cs_encoder *enc, size_t size, const void *val, size_t val_size)
{
   if (enc->len + size > enc->cap) {
      enc->cap = (enc->len + size) * 2 + 64;
      enc->buf = realloc(enc->buf, enc->cap);
   }
   memcpy(enc->buf + enc->len, val, val_size);
   memset(enc->buf + enc->len + val_size, 0, size - val_size);
   enc->len += size;
}

struct vn_cs_decoder {
   const uint8_t *cur;
   const uint8_t *end;
};

static inline void vn_cs_decoder_set_fatal(const struct vn_cs_decoder *dec) { (void)dec; abort(); }

static inline void
vn_cs_decoder_peek(const struct vn_cs_decoder *dec, size_t size, void *val, size_t val_size)
{
   (void)size;
   memcpy(val, dec->cur, val_size);
}

static inline void
vn_cs_decoder_read(struct vn_cs_decoder *dec, size_t size, void *val, size_t val_size)
{
   memcpy(val, dec->cur, val_size);
   dec->cur += size;
}

static inline uint64_t vn_cs_handle_load_id(const void **handle, VkObjectType type)
{
   (void)type;
   return (uint64_t)(uintptr_t)*handle;
}

static inline void vn_cs_handle_store_id(void **handle, uint64_t id, VkObjectType type)
{
   (void)type;
   *handle = (void *)(uintptr_t)id;
}

static inline bool vn_cs_renderer_protocol_has_extension(uint32_t ext) { (void)ext; return true; }
static inline bool vn_cs_renderer_protocol_has_api_version(uint32_t version) { (void)version; return true; }

struct vn_ring;
struct vn_ring_submit_command { int unused; };
#define VN_SUBMIT_LOCAL_CMD_SIZE 256
#define VN_TRACE_FUNC()
static inline struct vn_cs_encoder *
vn_ring_submit_command_init(struct vn_ring *ring, struct vn_ring_submit_command *submit, void *data, size_t size, size_t reply)
{
   (void)ring; (void)submit; (void)data; (void)size; (void)reply;
   return NULL;
}
static inline void vn_ring_submit_command(struct vn_ring *ring, struct vn_ring_submit_command *submit) { (void)ring; (void)submit; }
static inline struct vn_cs_decoder *vn_ring_get_command_reply(struct vn_ring *ring, struct vn_ring_submit_command *submit) { (void)ring; (void)submit; return NULL; }
static inline void vn_ring_free_command_reply(struct vn_ring *ring, struct vn_ring_submit_command *submit) { (void)ring; (void)submit; }

#endif
