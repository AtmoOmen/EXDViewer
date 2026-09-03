#include <dlfcn.h>
#include <pthread.h>
#include <stdio.h>
#include <stdlib.h>
#include <unistd.h>

#include "renderdoc_app.h"

static RENDERDOC_API_1_6_0 *rdoc;

/* The camera has to be where it should be before the frame is worth keeping, and the key hook does
   not reach through wine, so the capture waits on a file the shell can touch. */
static void *wait_for_sentinel(void *unused) {
    (void)unused;
    const char *sentinel = getenv("CAPTURE_WHEN");
    if (!sentinel) {
        return NULL;
    }
    unlink(sentinel);
    fprintf(stderr, "[capture] armed, waiting on %s\n", sentinel);
    for (;;) {
        if (access(sentinel, F_OK) == 0) {
            unlink(sentinel);
            rdoc->TriggerCapture();
            fprintf(stderr, "[capture] triggered\n");
        }
        usleep(200000);
    }
}

__attribute__((constructor)) static void arm(void) {
    void *lib = dlopen("librenderdoc.so", RTLD_NOW | RTLD_NOLOAD);
    if (!lib) {
        return;
    }
    pRENDERDOC_GetAPI get_api = (pRENDERDOC_GetAPI)dlsym(lib, "RENDERDOC_GetAPI");
    if (!get_api || get_api(eRENDERDOC_API_Version_1_6_0, (void **)&rdoc) != 1) {
        fprintf(stderr, "[capture] renderdoc loaded but its api is not 1.6\n");
        return;
    }
    pthread_t thread;
    pthread_create(&thread, NULL, wait_for_sentinel, NULL);
}
