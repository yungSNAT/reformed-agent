#[async_trait]
impl BrowserBackend for BrowserbaseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        cdp_navigate(self, url).await
    }
    async fn blank_page(&self) -> Result<String, ToolError> {
        cdp_blank_page(self).await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        cdp_snapshot(self).await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        cdp_click(self, selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        cdp_type(self, selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        cdp_scroll(self, direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        cdp_go_back(self).await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        cdp_press(self, key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        cdp_get_images(self, selector).await
    }
    async fn screenshot(&self) -> Result<String, ToolError> {
        cdp_screenshot(self).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        cdp_console(self, action).await
    }
}

#[async_trait]
impl BrowserBackend for BrowserUseBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        cdp_navigate(self, url).await
    }
    async fn blank_page(&self) -> Result<String, ToolError> {
        cdp_blank_page(self).await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        cdp_snapshot(self).await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        cdp_click(self, selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        cdp_type(self, selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        cdp_scroll(self, direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        cdp_go_back(self).await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        cdp_press(self, key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        cdp_get_images(self, selector).await
    }
    async fn screenshot(&self) -> Result<String, ToolError> {
        cdp_screenshot(self).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        cdp_console(self, action).await
    }
}

#[async_trait]
impl BrowserBackend for FirecrawlBrowserBackend {
    async fn navigate(&self, url: &str) -> Result<String, ToolError> {
        cdp_navigate(self, url).await
    }
    async fn blank_page(&self) -> Result<String, ToolError> {
        cdp_blank_page(self).await
    }
    async fn snapshot(&self) -> Result<String, ToolError> {
        cdp_snapshot(self).await
    }
    async fn click(&self, selector: &str) -> Result<String, ToolError> {
        cdp_click(self, selector).await
    }
    async fn r#type(&self, selector: &str, text: &str) -> Result<String, ToolError> {
        cdp_type(self, selector, text).await
    }
    async fn scroll(&self, direction: &str, amount: Option<u32>) -> Result<String, ToolError> {
        cdp_scroll(self, direction, amount).await
    }
    async fn go_back(&self) -> Result<String, ToolError> {
        cdp_go_back(self).await
    }
    async fn press(&self, key: &str) -> Result<String, ToolError> {
        cdp_press(self, key).await
    }
    async fn get_images(&self, selector: Option<&str>) -> Result<String, ToolError> {
        cdp_get_images(self, selector).await
    }
    async fn screenshot(&self) -> Result<String, ToolError> {
        cdp_screenshot(self).await
    }
    async fn console(&self, action: &str) -> Result<String, ToolError> {
        cdp_console(self, action).await
    }
}
