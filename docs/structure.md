目前流程大概是這樣，分成「輸入送進 PTY」、「ConPTY 讀回輸出」、「GUI 合併/
  渲染」三段。                                                              
                                                                            
  1. 使用者送出 gemini                                                      
  在 GuiState::submit_current_prompt() 裡：                                 
                                                                            
  1. UI prompt 內容被讀成 full_command。                                    
  2. 如果第一個 token 是 gemini | codex | claude | copilot，先呼叫          
     prepare_current_tab_for_interactive_ai()。                             
  3. 這會把 tab 切成 TerminalMode::InteractiveAi，清掉 terminal_lines、     
     interactive_history_lines、interactive_frame_lines、scroll 狀態。      
  4. 接著把 gemini 寫進目前 ConPTY stdin。                                  
  5. 寫完 Enter 後，送                                                      
     ControlCommand::SetRenderMode(ReaderRenderMode::InteractiveAi) 給      
     reader thread。                                                        
                                                                            
  所以 GUI 狀態會先切到 InteractiveAi，但 ConPTY reader 可能還有舊 Shell    
  snapshot 在 queue 裡，這就是之前加 stale shell filter 的原因。            
                                                                            
  2. ConPTY reader thread                                                   
  在 windows_conpty.rs::start_reader_thread()：                             
                                                                            
  1. byte thread 從 ConPTY stdout 讀 bytes。                                
  2. control thread 收 resize / render mode 指令。                          
  3. 主 reader loop 把 bytes 丟給 wezterm_term::Terminal::advance_bytes()。 
  4. 目前 terminal screen 存在 wezterm_term 內部 buffer。                   
  5. 每次內容變化後，reader 從 screen.lines_in_phys_range(start..end) 抽    
     snapshot。                                                             
  6. Shell mode 用 terminal_render_from_lines_cached()，只送 changed rows。 
  7. InteractiveAi mode 用 terminal_render_from_lines_full()，送整個        
     snapshot，changed_indices 是空的，代表「這批 lines 是 full snapshot」。  8. resize 時會 term.resize()、清 line cache、設 pending_reset = true，等  
     resize settle 後送一張 full snapshot。                                 
                                                                            
  重點：ConPTY/wezterm 這層沒有語意上的「訊息陣列」。它只有終端格子狀態，也 
  就是 TUI 畫面被重繪後的結果。                                             
                                                                            
  3. reader chunk 送到 GUI                                                  
  reader callback 會包成 TerminalChunk：                                    
                                                                            
  TerminalChunk {                                                           
      terminal_mode,                                                        
      lines,                                                                
      snapshot_len,                                                         
      full_len,                                                             
      first_line_idx,                                                       
      cursor_row,                                                           
      cursor_col,                                                           
      replace: true,                                                        
      changed_indices,                                                      
      reset_terminal_buffer,                                                
  }                                                                         
                                                                            
  然後經由 channel 進 spawn_terminal_stream_dispatcher()。                  
                                                                            
  dispatcher 會：                                                           
                                                                            
  1. 收很多 TerminalChunk 到 queue。                                        
  2. Slint callback on_terminal_data_ready drain queue。                    
  3. 每個 chunk 經 fold_chunk_into_pending()。                              
  4. 再進 apply_pending_updates() 寫入 TabState。                           
                                                                            
  4. GUI 端資料模型                                                         
  TabState 現在主要有三套：                                                 
                                                                            
  terminal_lines                                                            
  interactive_history_lines                                                 
  interactive_frame_lines                                                   
                                                                            
  含義是：                                                                  
                                                                            
  - interactive_frame_lines：目前 TUI snapshot，也就是 Gemini 畫面當前      
    frame。                                                                 
  - interactive_history_lines：我們自己推測出來的「已經掉出畫面的歷史」。   
  - terminal_lines：實際給 UI 顯示的資料，通常是 history + frame。          
                                                                            
  問題核心就在這裡：Gemini CLI 是 TUI，常常 repaint 同一個 frame。如果 GUI  
  把 repaint 當成新訊息 append 到 interactive_history_lines，就會重複。     
                                                                            
  5. InteractiveAi full snapshot 現在怎麼處理                               
  目前在 timers.rs::apply_pending_updates() 裡：                            
                                                                            
  如果是：                                                                  
                                                                            
  tab.terminal_mode == InteractiveAi                                        
  && update.changed_indices.is_empty()                                      
  && !new_lines.is_empty()                                                  
                                                                            
  代表 reader 給的是 full snapshot。                                        
                                                                            
  現在邏輯大致是：                                                          
                                                                            
  1. 如果 reset_terminal_buffer，清掉 history/frame/terminal_lines。        
  2. snapshot_lines = new_lines。                                           
  3. 如果 snapshot 裡有 CMD banner，做 preamble trim/drop。                 
  4. 截掉尾端空白。                                                         
  5. 不再把整張 snapshot append 到 history。                                
  6. 只把 snapshot_lines 設成 interactive_frame_lines。                     
  7. 用 compose_interactive_terminal_lines() 組成 terminal_lines = history +     frame。                                                                
  8. cursor row 轉成 history_len + frame_row。                              
                                                                            
  這是最近修正的方向。                                                      
                                                                            
  6. resize 流程                                                            
  resize 目前是：                                                           
                                                                            
  1. Slint 觸發 on_terminal_resize_requested(cols, rows)。                  
  2. GUI debounce 約 140ms。                                                
  3. 呼叫 Windows ConPTY ResizePseudoConsole()。                            
  4. 同時送 ControlCommand::Resize 給 reader。                              
  5. reader 呼叫 wezterm_term::Terminal::resize()。                         
  6. reader 設 pending_reset = true，暫停輸出直到 resize settle。           
  7. settle 後送一張 reset_terminal_buffer = true 的 full snapshot。        
  8. GUI 看到 reset，在 InteractiveAi 模式清掉舊 history/frame，拿新        
     snapshot 當 baseline。                                                 
                                                                            
  7. scrollbar 怎麼來                                                       
  scrollbar 不是 ConPTY 給的，是 GUI 根據 terminal_lines.len() 算的。       
                                                                            
  流程：                                                                    
                                                                            
  1. compose_interactive_terminal_lines() 更新 tab.terminal_lines。         
  2. refresh_current_terminal() 算 scrollable_terminal_line_count(tab)。    
  3. 設定 ui.set_ws_terminal_total_lines(n)。                               
  4. push_terminal_view_to_ui() 只把可視範圍那幾行 push 到 Slint model。    
                                                                            
  所以 scrollbar 消失通常代表：                                             
                                                                            
  - terminal_lines 被清空或變短。                                           
  - 或 scrollable_terminal_line_count() 因 pinned footer 算出 body rows 很  
    少。                                                                    
  - 或 UI 沒有收到新的 total_lines/model refresh。                          
                                                                            
  目前最關鍵的結論                                                          
  現在這套架構在嘗試從「TUI 畫面 snapshot」反推出「append-only 訊息歷史」。 
  這本質上很不穩，因為 Gemini 不是逐行 log，而是反覆重畫同一個終端畫面。    
                                                                            
  所以正確模型應該分清楚：                                                  
                                                                            
  Shell mode:                                                               
  ConPTY scrollback snapshot -> terminal_lines
                                                                            
  Interactive TUI mode:                                                     
  current_frame snapshot -> 畫面                                            
  不要自動 append repaint 到 history                                        
                                                                            
  真正 history:                                                             
  只能從掉出 viewport 的 rows 或語意來源取得                                
                                                                            
  也就是說，對 Gemini 這種 TUI，最安全的是「目前畫面優先正確」，不要強行做  
  append-only 訊息歷史。否則 resize / redraw / loading spinner / waiting    
  auth 都會被當成新訊息。                      