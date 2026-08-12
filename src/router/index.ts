import { createRouter, createWebHashHistory } from "vue-router";

const router = createRouter({
  history: createWebHashHistory(),
  routes: [
    { path: "/setup", component: () => import("../pages/SetupWizardPage.vue") },
    { path: "/", component: () => import("../pages/PreflightPage.vue") },
    { path: "/pilot", component: () => import("../pages/PilotPage.vue") },
    { path: "/batch", component: () => import("../pages/BatchPage.vue") },
    { path: "/report/:id", component: () => import("../pages/ReportPage.vue") },
  ],
});

export default router;
